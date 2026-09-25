//! Whisper transcription segments, and the dual/solo merge into
//! `transcript.md`.
//!
//! One `Segment` is one line whisper.cpp's JSON output mode reports (a
//! chunk of text with a start offset in the source audio, milliseconds).
//! `merge_dual` interleaves two tracks' segment streams by timestamp into
//! the `**You** (mm:ss): ...` / `**Them** (mm:ss): ...` shape `mach meet
//! process` writes for a dual-mode meeting; `merge_solo` drops the
//! speaker label a solo (mic-only) meeting has no use for, keeping just
//! the timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// Offset from the start of this track's recording, milliseconds --
    /// whisper.cpp's own `offsets.from` field (see `transcribe`'s JSON
    /// parser). Both tracks start recording within the same `mach meet
    /// start` call (mic spawned, then system, back-to-back), so their
    /// offsets are directly comparable without any clock adjustment --
    /// close enough for meeting notes, not frame-accurate sync.
    pub start_ms: u64,
    pub text: String,
}

/// `mm:ss`, truncating anything past an hour into the minutes field rather
/// than rolling over -- meetings this long are unusual but must still
/// render a sane (if wide) timestamp instead of wrapping silently.
pub fn format_mmss(ms: u64) -> String {
    let total_secs = ms / 1000;
    format!("{:02}:{:02}", total_secs / 60, total_secs % 60)
}

#[derive(Clone, Copy)]
enum Who {
    You,
    Them,
}

impl Who {
    fn label(self) -> &'static str {
        match self {
            Who::You => "You",
            Who::Them => "Them",
        }
    }
}

/// Interleaves `mine` (the mic track -- "You") and `theirs` (the system
/// track -- "Them") into one chronological transcript, each line `**<Who>**
/// (mm:ss): <text>`. Both inputs are assumed already in their own
/// chronological order (whisper.cpp emits segments in order); the merge is
/// a stable sort on `start_ms` alone, so an exact-tie keeps `mine` before
/// `theirs` -- arbitrary but deterministic, and irrelevant in practice since
/// two real speech segments essentially never share a millisecond-exact
/// start. Blank/whitespace-only segments (whisper occasionally emits one
/// for a silence gap) are dropped rather than rendered as an empty line.
///
/// Mic segments that are just the remote side leaking through speakers
/// into the mic are dropped first (see `is_speaker_bleed`) -- without
/// that, most remote lines appear twice, once under each label.
pub fn merge_dual(mine: &[Segment], theirs: &[Segment]) -> String {
    let mut all: Vec<(Who, &Segment)> = Vec::with_capacity(mine.len() + theirs.len());
    all.extend(
        mine.iter()
            .enumerate()
            .filter(|(i, s)| !is_speaker_bleed(s, mine.get(i + 1), theirs))
            .map(|(_, s)| (Who::You, s)),
    );
    all.extend(theirs.iter().map(|s| (Who::Them, s)));
    all.sort_by_key(|(_, s)| s.start_ms);

    let mut out = String::new();
    for (who, seg) in all {
        let text = seg.text.trim();
        if text.is_empty() {
            continue;
        }
        out.push_str(&format!("**{}** ({}): {}\n", who.label(), format_mmss(seg.start_ms), text));
    }
    out
}

/// How far before a mic segment's start a system segment may begin and
/// still count as the same speech: the two recorders start back-to-back
/// and whisper splits each track at different points.
const BLEED_LEAD_MS: u64 = 3_000;
/// Upper bound on a mic segment's span when there is no next mic segment
/// to end it (whisper reports start offsets only).
const BLEED_MAX_SPAN_MS: u64 = 15_000;
/// Fraction of a mic segment's words that must also appear on the system
/// track in its time window for it to count as bleed. Measured on a real
/// 48-minute speakers-on meeting: bleed lines score >= 0.7 almost always,
/// the user's own lines <= 0.35.
const BLEED_MIN_OVERLAP: f64 = 0.6;

fn words(text: &str) -> std::collections::HashSet<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// True when `seg` (a mic segment) is the system track's audio picked up
/// again by the mic -- speakers instead of headphones. Its window runs
/// from `BLEED_LEAD_MS` before its start to the next mic segment's start
/// (capped at `BLEED_MAX_SPAN_MS`), and it is bleed when at least
/// `BLEED_MIN_OVERLAP` of its words occur in system segments starting
/// inside that window.
fn is_speaker_bleed(seg: &Segment, next: Option<&Segment>, theirs: &[Segment]) -> bool {
    let mine = words(&seg.text);
    if mine.is_empty() {
        return false;
    }
    let from = seg.start_ms.saturating_sub(BLEED_LEAD_MS);
    let to = next
        .map(|n| n.start_ms)
        .unwrap_or(u64::MAX)
        .min(seg.start_ms + BLEED_MAX_SPAN_MS)
        .max(seg.start_ms);
    let heard: std::collections::HashSet<String> = theirs
        .iter()
        .filter(|t| t.start_ms >= from && t.start_ms <= to)
        .flat_map(|t| words(&t.text))
        .collect();
    let shared = mine.iter().filter(|w| heard.contains(*w)).count();
    shared as f64 / mine.len() as f64 >= BLEED_MIN_OVERLAP
}

/// A solo (mic-only) meeting's transcript: one unlabeled stream, timestamp
/// kept (still useful for finding a moment in the recording) but no
/// `**You**`/`**Them**` prefix -- there's only one speaker, and a fixed
/// label for it would just be noise on every line.
pub fn merge_solo(segments: &[Segment]) -> String {
    let mut out = String::new();
    for seg in segments {
        let text = seg.text.trim();
        if text.is_empty() {
            continue;
        }
        out.push_str(&format!("({}): {}\n", format_mmss(seg.start_ms), text));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start_ms: u64, text: &str) -> Segment {
        Segment { start_ms, text: text.to_string() }
    }

    #[test]
    fn format_mmss_pads_and_computes_minutes_seconds() {
        assert_eq!(format_mmss(0), "00:00");
        assert_eq!(format_mmss(59_000), "00:59");
        assert_eq!(format_mmss(60_000), "01:00");
        assert_eq!(format_mmss(125_000), "02:05");
    }

    #[test]
    fn format_mmss_truncates_sub_second_precision() {
        assert_eq!(format_mmss(59_999), "00:59");
    }

    #[test]
    fn merge_dual_interleaves_by_timestamp() {
        let mine = vec![seg(0, "hello there"), seg(10_000, "how are you")];
        let theirs = vec![seg(5_000, "hi"), seg(15_000, "good thanks")];
        let out = merge_dual(&mine, &theirs);
        assert_eq!(
            out,
            "**You** (00:00): hello there\n\
             **Them** (00:05): hi\n\
             **You** (00:10): how are you\n\
             **Them** (00:15): good thanks\n"
        );
    }

    #[test]
    fn merge_dual_ties_keep_mine_before_theirs() {
        let mine = vec![seg(1_000, "mine")];
        let theirs = vec![seg(1_000, "theirs")];
        let out = merge_dual(&mine, &theirs);
        assert_eq!(out, "**You** (00:01): mine\n**Them** (00:01): theirs\n");
    }

    #[test]
    fn merge_dual_drops_blank_segments() {
        let mine = vec![seg(0, "  "), seg(1_000, "real text")];
        let theirs = vec![seg(500, "")];
        let out = merge_dual(&mine, &theirs);
        assert_eq!(out, "**You** (00:01): real text\n");
    }

    #[test]
    fn merge_dual_drops_mic_lines_that_echo_the_system_track() {
        // speakers leaking into the mic: the mic hears the remote side and
        // whisper transcribes it again, a little later and differently split
        let mine = vec![seg(1_000, "And I'll be in Turkey for the ISC around."), seg(20_000, "let me share my screen")];
        let theirs = vec![seg(0, "and I'll be in Turkey,"), seg(2_000, "uh, for the ISC,"), seg(4_000, "around, uh,")];
        let out = merge_dual(&mine, &theirs);
        assert_eq!(
            out,
            "**Them** (00:00): and I'll be in Turkey,\n\
             **Them** (00:02): uh, for the ISC,\n\
             **Them** (00:04): around, uh,\n\
             **You** (00:20): let me share my screen\n"
        );
    }

    #[test]
    fn merge_dual_keeps_mic_line_when_system_says_same_words_much_later() {
        let mine = vec![seg(0, "sounds good to me")];
        let theirs = vec![seg(60_000, "sounds good to me")];
        let out = merge_dual(&mine, &theirs);
        assert_eq!(out, "**You** (00:00): sounds good to me\n**Them** (01:00): sounds good to me\n");
    }

    #[test]
    fn merge_dual_keeps_mic_line_with_little_overlap() {
        let mine = vec![seg(0, "what do you think is the best idea here")];
        let theirs = vec![seg(1_000, "I think the idea")];
        let out = merge_dual(&mine, &theirs);
        assert!(out.contains("**You** (00:00): what do you think is the best idea here\n"));
    }

    #[test]
    fn merge_dual_empty_inputs_yield_empty_transcript() {
        assert_eq!(merge_dual(&[], &[]), "");
    }

    #[test]
    fn merge_solo_has_no_speaker_label() {
        let segs = vec![seg(0, "one"), seg(3_000, "two")];
        let out = merge_solo(&segs);
        assert_eq!(out, "(00:00): one\n(00:03): two\n");
    }

    #[test]
    fn merge_solo_drops_blank_segments() {
        let segs = vec![seg(0, "   "), seg(1_000, "kept")];
        assert_eq!(merge_solo(&segs), "(00:01): kept\n");
    }
}
