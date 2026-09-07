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
pub fn merge_dual(mine: &[Segment], theirs: &[Segment]) -> String {
    let mut all: Vec<(Who, &Segment)> = Vec::with_capacity(mine.len() + theirs.len());
    all.extend(mine.iter().map(|s| (Who::You, s)));
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
