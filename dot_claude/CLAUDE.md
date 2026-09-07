# Global Rules

These apply to EVERY project, no exceptions.

## Secrets handling — DO NOT FUCK UP

### Never write real secret values into the working tree.

This includes — but is not limited to — `.env`, `config.toml`, `deploy/`,
`infra/`, `scripts/`, `terraform/`, any file under a tracked directory.

Even if you intend to gitignore it. Even if "it's just for testing." Even
if the user pasted the secret to you. **The working tree leaks** —
through rsync, accidental commits, IDE indexing, shell history,
backups, screen-shares, AI transcripts.

Always:
1. **Create the `.gitignore` entry first** — before the file exists.
2. **Write `<name>.example`** with placeholder values (e.g. `CHANGEME_*`).
3. **Tell the user to populate the real file at runtime**, outside the
   repo, in `~/.config/...` or via an env-injection mechanism.

If the user pastes you a real secret directly, treat it as radioactive:
acknowledge receipt, store only in volatile session memory, never write
it to disk. Suggest they use a secret manager instead.

### Never echo real secrets in chat output.

Not in suggested commands. Not in config blocks. Not in explanations.
Not in error analysis. Replace with `<your-pat>` / `<jwt-secret>` /
generic placeholders. **The transcript persists and is itself a leak
surface.**

If you've already typed a real secret and noticed: rotate it
immediately, don't try to retract.

### rsync to a remote that holds production secrets MUST exclude any
### local file that mirrors a remote secret.

Default `--exclude` set:
- `*.env*`
- `**/secrets/**`
- `**/zenith-config/**` (or whatever the per-deploy secret dir is named)
- `**/config.toml` that lives outside `~/.config/`

Better: rsync should ship code only. Config-bearing files live on the
target, edited via a hot-reload mechanism or admin tooling. Never have
a local "deploy config" that mirrors the live one — it WILL get
clobbered, both directions.

### When a secret leak is suspected, ORDER MATTERS:

1. **Contain** — delete the leaking file from the working tree.
2. **Verify** — `git grep` for known token prefixes across all reachable
   commits (`git rev-list --all`). Check working tree too.
3. **Scrub if anything in git history** — install `git-filter-repo`
   (system package; see below), drop the path with
   `--invert-paths --path X`, force-push all affected refs.
4. **Then report** to the user. Tell them every place the secret lived:
   working tree, git history (before scrub), remote hosts, this
   transcript, GitHub web cache (mention they may need a Support ticket
   to expedite GH's GC).
5. **Always remind them to rotate**, because scrubbing doesn't unleak
   what's already been seen.

DO NOT apologize first and propose cleanup steps for the user to run.
Run them. Then describe.

## Package install discipline

### Arch Linux: pacman first.

Before reaching for `pip`, `npm install -g`, `cargo install`, or any
language-level installer that needs to touch a managed Python/Node/Rust
toolchain — **check pacman first**:

```
pacman -Si <pkg>          # is it in [extra] or [core]?
paru -Si <pkg>            # AUR fallback if available
```

If it's there, use `sudo pacman -S <pkg>` (ask the user to run if you
don't have sudo).

### NEVER use `pip install --break-system-packages`.

PEP 668 marked these environments managed for a reason. The flag exists
as a foot-gun, not a tool. If pacman doesn't have it and the user wants
it, prefer in this order:
1. `pacman` / AUR
2. `pipx install <pkg>` — for end-user CLIs
3. A venv: `python -m venv .venv && .venv/bin/pip install <pkg>`
4. Never the break-system-packages flag.

Same principle applies on Debian/Ubuntu (`apt` first) and Fedora
(`dnf` first).

## Destructive ops require confirmation in auto mode too.

"Auto mode" doesn't unlock destructive actions on shared/production
state — only routine reversible work. Always pause for:
- `git push --force` / history rewrites against remotes the user shares
- `rm -rf` of directories with unique unbacked-up content
- DB/MinIO/storage wipes
- `sudo` invocations
- Writes that overwrite a config-of-record holding live secrets

Confirm scope explicitly. "User said 'do the cleanup' three turns ago"
is NOT a standing authorization for the next destructive step.

## When the user says "global memory" they mean THIS file.

`~/.claude/CLAUDE.md` is the cross-project rules surface. Anything
project-specific goes in the per-project memory under
`~/.claude/projects/<slug>/memory/`. Don't confuse the two.

## Knowledge bank

`mach kb` is a personal, vectorized, cross-session fact store
(`~/.local/share/mach/kb.db`). A hook already searches it on every prompt
and injects close matches as context — you don't have to ask for that.
Save durable user-facts deliberately, don't wait to be asked: `mach kb add
"<fact>" --source "<context>"`. The `kb` skill covers what qualifies. Same
rule as everywhere else in this file: **never store secrets or
credentials in it.** Auto-extracted candidates land unreviewed —
`mach kb review` is how the user curates that queue, not you unprompted.
When a prompt arrives with injected kb recall that already answers it,
answer from the recall first — don't reach for file exploration to
rediscover what the bank just told you. Explore to VERIFY or extend,
not to re-derive.
