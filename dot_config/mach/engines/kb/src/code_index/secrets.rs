//! Code index: secret filtering. Standing rule: the knowledge bank must
//! never store a secret -- not as a chunk, not in a header prompt sent to
//! the LLM (and so in `judge_log`), not in a `read` tool result.
//!
//! Two checks, both deliberately conservative (a skipped legitimate file
//! costs one missing file in the index; a stored secret is a leak):
//!
//! - [`path_reason`]: filename patterns that almost always hold secrets
//!   (`.env`, `.env.*`, `*.pem`, `*.key`, `*.p12`, `*.pfx`, `id_rsa*`,
//!   `id_ed25519*`, `*.kdbx`, `*.keystore`), plus any path component
//!   containing `credentials` or `secret`. Checked before a file's content
//!   is even read.
//! - [`content_reason`]: high-confidence secret markers in the text itself
//!   (PEM private-key blocks, GitHub/OpenAI-style/Anthropic/Slack/AWS/Google
//!   API tokens, AWS secret access keys, JWTs, literal password
//!   assignments).
//!
//! Both return a short *kind* label (e.g. `secret-path(*.pem)`,
//! `secret-content(github-token)`) that is safe to store and print -- it
//! names the pattern, never the matched value.

/// `Some(reason)` when `path` looks like a secret-bearing file.
pub fn path_reason(path: &str) -> Option<&'static str> {
    let lower = path.to_ascii_lowercase();
    let base = lower.rsplit('/').next().unwrap_or(&lower);
    if base == ".env" || base.starts_with(".env.") {
        return Some("secret-path(.env)");
    }
    for (ext, label) in [
        (".pem", "secret-path(*.pem)"),
        (".key", "secret-path(*.key)"),
        (".p12", "secret-path(*.p12)"),
        (".pfx", "secret-path(*.pfx)"),
        (".kdbx", "secret-path(*.kdbx)"),
        (".keystore", "secret-path(*.keystore)"),
    ] {
        if base.ends_with(ext) {
            return Some(label);
        }
    }
    if base.starts_with("id_rsa") {
        return Some("secret-path(id_rsa*)");
    }
    if base.starts_with("id_ed25519") {
        return Some("secret-path(id_ed25519*)");
    }
    for comp in lower.split('/') {
        if comp.contains("credentials") {
            return Some("secret-path(*credentials*)");
        }
        if comp.contains("secret") {
            return Some("secret-path(*secret*)");
        }
    }
    None
}

fn is_token_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// True when the byte before `idx` (if any) is not part of an identifier --
/// so `ghp_` inside `foo_ghp_bar` is not a token start.
fn at_word_start(bytes: &[u8], idx: usize) -> bool {
    idx == 0 || !is_token_char(bytes[idx - 1])
}

/// Length of the run starting at `from` whose bytes satisfy `ok`.
fn run_len(bytes: &[u8], from: usize, ok: impl Fn(u8) -> bool) -> usize {
    bytes[from.min(bytes.len())..].iter().take_while(|&&b| ok(b)).count()
}

/// Every word-start occurrence of `prefix` in `text`, as the byte index just
/// past the prefix.
fn prefix_hits<'a>(text: &'a str, prefix: &'a str) -> impl Iterator<Item = usize> + 'a {
    let bytes = text.as_bytes();
    text.match_indices(prefix).filter(move |(i, _)| at_word_start(bytes, *i)).map(move |(i, _)| i + prefix.len())
}

/// `Some(kind)` when `text` contains a high-confidence secret marker.
pub fn content_reason(text: &str) -> Option<&'static str> {
    let bytes = text.as_bytes();

    // PEM private key: "-----BEGIN ... PRIVATE KEY-----" on one line.
    for (i, _) in text.match_indices("-----BEGIN") {
        let rest = &text[i..];
        let line = rest.split('\n').next().unwrap_or(rest);
        if line.contains("PRIVATE KEY-----") {
            return Some("secret-content(private-key)");
        }
    }

    // GitHub tokens: ghp_/gho_/github_pat_ + 20 or more token chars.
    for prefix in ["ghp_", "gho_", "github_pat_"] {
        if prefix_hits(text, prefix).any(|end| run_len(bytes, end, is_token_char) >= 20) {
            return Some("secret-content(github-token)");
        }
    }

    // sk- API keys: sk- + 20 alnum, or the dashed sk-ant-/sk-proj- forms.
    for end in prefix_hits(text, "sk-") {
        if run_len(bytes, end, |b| b.is_ascii_alphanumeric()) >= 20 {
            return Some("secret-content(sk-api-key)");
        }
        for sub in ["ant-", "proj-"] {
            if text[end..].starts_with(sub)
                && run_len(bytes, end + sub.len(), |b| is_token_char(b) || b == b'-') >= 20
            {
                return Some("secret-content(sk-api-key)");
            }
        }
    }

    // Slack: xox[baprs]- + 10 or more token chars.
    for kind in [b'b', b'a', b'p', b'r', b's'] {
        let prefix = format!("xox{}-", kind as char);
        if prefix_hits(text, &prefix).any(|end| run_len(bytes, end, |b| is_token_char(b) || b == b'-') >= 10) {
            return Some("secret-content(slack-token)");
        }
    }

    // AWS access key id: AKIA + exactly 16 of [0-9A-Z], not part of a longer word.
    for end in prefix_hits(text, "AKIA") {
        let n = run_len(bytes, end, |b| b.is_ascii_digit() || b.is_ascii_uppercase());
        if n == 16 && !bytes.get(end + n).is_some_and(|&b| is_token_char(b)) {
            return Some("secret-content(aws-access-key)");
        }
    }

    // Google API key: AIza + 35 of [0-9A-Za-z_-].
    for end in prefix_hits(text, "AIza") {
        if run_len(bytes, end, |b| is_token_char(b) || b == b'-') >= 35 {
            return Some("secret-content(google-api-key)");
        }
    }

    // JWT: eyJ<b64url>.eyJ<b64url>.
    let b64 = |b: u8| is_token_char(b) || b == b'-';
    for end in prefix_hits(text, "eyJ") {
        let n1 = run_len(bytes, end, b64);
        let dot = end + n1;
        if n1 >= 4 && bytes.get(dot) == Some(&b'.') && text[dot + 1..].starts_with("eyJ") {
            let n2 = run_len(bytes, dot + 4, b64);
            if n2 >= 4 && bytes.get(dot + 4 + n2) == Some(&b'.') {
                return Some("secret-content(jwt)");
            }
        }
    }

    // AWS secret access key: 40 base64 chars shortly after an `aws_secret`
    // label (`aws_secret_access_key = ...`, `AWS_SECRET_ACCESS_KEY: ...`).
    let lower = text.to_ascii_lowercase();
    for (i, _) in lower.match_indices("aws_secret") {
        let window_end = (i + 80).min(bytes.len());
        let mut j = i + "aws_secret".len();
        // Skip the rest of the label and the separator.
        while j < window_end && (is_token_char(bytes[j]) || matches!(bytes[j], b' ' | b'\t' | b'=' | b':' | b'"' | b'\'')) {
            let n = run_len(bytes, j, |b| b.is_ascii_alphanumeric() || b == b'/' || b == b'+');
            if n == 40 && (j == 0 || !is_b64(bytes[j - 1])) && !bytes.get(j + n).is_some_and(|&b| is_b64(b)) {
                return Some("secret-content(aws-secret-key)");
            }
            j += 1;
        }
    }

    // Generic password assignment: `password` then optional whitespace then
    // `:` or `=` (not `==`/`=>`) then a literal value -- a quoted non-empty
    // string, or a bare token of 6+ chars that contains a digit or symbol
    // and isn't a call/expression. Deliberately value-shaped so a struct
    // field (`password: String`), a comparison (`password == x`) or a
    // lookup (`password = get_password()`) isn't flagged.
    for (i, _) in lower.match_indices("password") {
        let mut j = i + "password".len();
        // `password_hash = ...` etc. is a different identifier.
        if bytes.get(j).is_some_and(|&b| is_token_char(b)) {
            continue;
        }
        while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'"' | b'\'') {
            j += 1;
        }
        if j >= bytes.len() || !matches!(bytes[j], b':' | b'=') {
            continue;
        }
        if bytes.get(j + 1).is_some_and(|&b| b == b'=' || b == b'>') {
            continue;
        }
        j += 1;
        while j < bytes.len() && matches!(bytes[j], b' ' | b'\t') {
            j += 1;
        }
        if password_value_is_literal(&bytes[j..]) {
            return Some("secret-content(password)");
        }
    }

    None
}

fn is_b64(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'/' || b == b'+' || b == b'='
}

/// See the `password` block in [`content_reason`].
fn password_value_is_literal(rest: &[u8]) -> bool {
    match rest.first() {
        Some(&q) if q == b'"' || q == b'\'' => {
            let n = rest[1..].iter().take_while(|&&b| b != q && b != b'\n').count();
            // Closed, non-empty, and not a template placeholder.
            rest.get(1 + n) == Some(&q) && n >= 1 && !matches!(rest[1], b'$' | b'{' | b'<' | b'%')
        }
        Some(_) => {
            let tok: Vec<u8> = rest.iter().take_while(|&&b| !b.is_ascii_whitespace() && !matches!(b, b',' | b';' | b')')).copied().collect();
            tok.len() >= 6
                && !matches!(tok[0], b'$' | b'{' | b'<' | b'%' | b'(' | b'[' | b'&' | b'*')
                && !tok.contains(&b'(')
                && tok.iter().any(|b| b.is_ascii_digit() || b"!@#^+/=_-".contains(b))
                && !tok.iter().all(|b| is_token_char(*b) && !b.is_ascii_digit())
        }
        None => false,
    }
}

/// `path_reason` first, then (only if the path is clean) `content_reason`.
pub fn reason(path: &str, text: &str) -> Option<&'static str> {
    path_reason(path).or_else(|| content_reason(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every fake token below is assembled at runtime from harmless pieces,
    // so no real-looking secret literal ever sits in this source file.
    fn rep(c: char, n: usize) -> String {
        std::iter::repeat(c).take(n).collect()
    }

    #[test]
    fn path_patterns_flag_the_listed_secret_files() {
        for p in [
            ".env",
            "deploy/.env",
            ".env.production",
            "certs/server.pem",
            "tls/server.KEY",
            "a/b.p12",
            "b.pfx",
            "home/.ssh/id_rsa",
            "id_rsa.pub",
            "id_ed25519",
            "config/credentials.json",
            "aws_credentials",
            "src/secrets/mod.rs",
            "client_secret.json",
            "vault.kdbx",
            "release.keystore",
        ] {
            assert!(path_reason(p).is_some(), "{} must be flagged", p);
        }
    }

    #[test]
    fn path_patterns_leave_ordinary_source_alone() {
        for p in ["src/main.rs", "env.rs", "src/environment.cpp", ".envrc_docs.md", "keyboard.c", "monkey.py", "README.md", "src/keys.rs"] {
            assert_eq!(path_reason(p), None, "{} must not be flagged", p);
        }
    }

    #[test]
    fn content_markers_flag_each_token_kind() {
        let pem = format!("{}BEGIN RSA PRIVATE KEY{}\nMIIE...\n", rep('-', 5), rep('-', 5));
        let pem_openssh = format!("x\n{}BEGIN OPENSSH PRIVATE KEY{}\n", rep('-', 5), rep('-', 5));
        let ghp = format!("token = \"{}{}\"", "gh".to_string() + "p_", rep('a', 36));
        let gho = format!("{}{}", "gh".to_string() + "o_", rep('Z', 30));
        let pat = format!("t={}{}", "github".to_string() + "_pat_", rep('1', 40));
        let sk = format!("KEY={}{}", "s".to_string() + "k-", rep('x', 24));
        let sk_ant = format!("{}{}{}", "s".to_string() + "k-", "ant-api03-", rep('Q', 40));
        let slack = format!("{}{}-{}", "xo".to_string() + "xb-", rep('1', 12), rep('a', 20));
        let aws = format!("id: {}{}", "AK".to_string() + "IA", rep('Q', 16));
        let google = format!("{}{}", "AI".to_string() + "za", rep('b', 35));
        let jwt = format!("Bearer {}{}.{}{}.sig", "ey".to_string() + "J", rep('h', 20), "ey".to_string() + "J", rep('p', 30));
        for (label, text) in [
            ("pem", pem),
            ("openssh", pem_openssh),
            ("ghp", ghp),
            ("gho", gho),
            ("github_pat", pat),
            ("sk", sk),
            ("sk-ant", sk_ant),
            ("slack", slack),
            ("aws", aws),
            ("google", google),
            ("jwt", jwt),
        ] {
            assert!(content_reason(&text).is_some(), "{} marker must be detected", label);
        }
    }

    #[test]
    fn content_markers_ignore_near_misses_in_ordinary_code() {
        let short_ghp = format!("{}abc", "gh".to_string() + "p_");
        let embedded = format!("my{}{}", "gh".to_string() + "p_", rep('a', 36));
        let aws_long = format!("{}{}", "AK".to_string() + "IA", rep('Q', 20));
        for text in [
            "fn main() { println!(\"hello\"); }".to_string(),
            "-----BEGIN PUBLIC KEY-----\nMIIB\n-----END PUBLIC KEY-----".to_string(),
            "task-scheduler-with-a-long-name-here".to_string(),
            short_ghp,
            embedded,
            aws_long,
            "let x = eyJ;".to_string(),
        ] {
            assert_eq!(content_reason(&text), None, "{:?} must not be flagged", text);
        }
    }

    #[test]
    fn content_markers_flag_aws_secret_keys_and_password_literals() {
        let secret40 = format!("{}{}{}", rep('A', 13), "/b+", rep('9', 24));
        assert_eq!(secret40.len(), 40);
        let aws = format!("aws_secret_access_key = {}", secret40);
        let aws_env = format!("AWS_SECRET_ACCESS_KEY: \"{}\"", secret40);
        let pw_quoted = format!("pass{} = \"{}\"", "word", "hunter2");
        let pw_bare = format!("db_pass{}: {}", "word", "s3cr3t-value");
        let pw_colon = format!("PASS{}=Tr0ub4dor", "WORD");
        for (label, text) in [("aws", aws), ("aws-env", aws_env), ("pw-quoted", pw_quoted), ("pw-bare", pw_bare), ("pw-colon", pw_colon)] {
            assert!(content_reason(&text).is_some(), "{} must be detected: {}", label, text);
        }
        assert_eq!(content_reason(&format!("aws_secret_access_key = {}", secret40)), Some("secret-content(aws-secret-key)"));
    }

    #[test]
    fn password_marker_ignores_fields_comparisons_and_lookups() {
        for text in [
            "struct Login { password: String }",
            "if password == expected { ok() }",
            "let password = read_password();",
            "password: Option<String>,",
            "def check(password): pass",
            "password_hash = bcrypt(pw)",
            "password = \"\"",
            "password: \"${DB_PASSWORD}\"",
            "the password is required",
            "aws_secret_access_key = os.environ['X']",
        ] {
            assert_eq!(content_reason(text), None, "{:?} must not be flagged", text);
        }
    }

    #[test]
    fn the_reason_names_the_pattern_never_the_value() {
        let token = format!("{}{}", "gh".to_string() + "p_", rep('a', 36));
        let r = content_reason(&token).unwrap();
        assert!(!r.contains(&token[4..]), "{}", r);
        assert_eq!(r, "secret-content(github-token)");
    }
}
