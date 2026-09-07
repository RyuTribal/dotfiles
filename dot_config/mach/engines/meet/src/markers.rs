//! The marker-file state machine `mach meet process` drives per meeting
//! directory: `needs-processing` (written by `mach meet stop`) ->
//! `needs-summary` (written once transcription finishes) -> `processed`
//! (written once the sonnet call finishes). Pulled out as a pure function
//! of booleans -- no filesystem, no subprocess -- so every transition,
//! including both offline-deferral cases, is unit-testable without a real
//! meeting directory, a real whisper.cpp binary, or a real `claude`
//! process. `process::process_dir` is the thin imperative shell that reads
//! the actual marker files and tool availability and hands the results to
//! `decide_stage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageAction {
    /// `needs-processing` is set and a whisper.cpp binary/model is
    /// available -- attempt transcription + merge.
    Transcribe,
    /// `needs-processing` is set but no whisper.cpp binary/model could be
    /// resolved -- the "laptop/AUR reality" degradation: leave the marker
    /// exactly as it is (nothing lost, retried on the next opportunistic
    /// wakeup), log/notify, and stop for this meeting.
    TranscriptionToolingMissing,
    /// `needs-summary` is set (transcription already succeeded) and
    /// `claude` is reachable -- attempt the summarize/label/extract call.
    Summarize,
    /// `needs-summary` is set but `claude` is unreachable (offline laptop)
    /// -- leave the marker, defer to the next opportunistic wakeup. This is
    /// the split this task calls for: transcription (local, offline-safe)
    /// already completed and is never re-attempted; only the network-
    /// dependent claude step is deferred.
    SummarizeDeferred,
    /// Neither marker is present (already `processed`, or a directory with
    /// no marker at all) -- nothing to do.
    Done,
}

/// `has_needs_processing` and `has_needs_summary` reflect which marker
/// file(s) actually exist in the meeting directory right now (ordinarily
/// exactly one, or neither once a meeting is `processed` -- but this makes
/// no assumption about that and always prioritizes transcription first:
/// a directory can never be in a state where summarization is attempted
/// before its transcript exists).
pub fn decide_stage(has_needs_processing: bool, has_needs_summary: bool, whisper_available: bool, claude_online: bool) -> StageAction {
    if has_needs_processing {
        return if whisper_available { StageAction::Transcribe } else { StageAction::TranscriptionToolingMissing };
    }
    if has_needs_summary {
        return if claude_online { StageAction::Summarize } else { StageAction::SummarizeDeferred };
    }
    StageAction::Done
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_processing_with_whisper_available_transcribes() {
        assert_eq!(decide_stage(true, false, true, true), StageAction::Transcribe);
        // claude availability is irrelevant at this stage.
        assert_eq!(decide_stage(true, false, true, false), StageAction::Transcribe);
    }

    #[test]
    fn needs_processing_without_whisper_defers_with_a_distinct_action() {
        assert_eq!(decide_stage(true, false, false, true), StageAction::TranscriptionToolingMissing);
    }

    #[test]
    fn needs_processing_takes_priority_over_needs_summary() {
        // Should be an impossible real-world state (only one marker exists
        // at a time), but the decision must still never attempt to
        // summarize before transcription is confirmed done.
        assert_eq!(decide_stage(true, true, true, true), StageAction::Transcribe);
    }

    #[test]
    fn needs_summary_with_claude_online_summarizes() {
        assert_eq!(decide_stage(false, true, true, true), StageAction::Summarize);
        // whisper availability is irrelevant at this stage.
        assert_eq!(decide_stage(false, true, false, true), StageAction::Summarize);
    }

    #[test]
    fn needs_summary_offline_defers() {
        assert_eq!(decide_stage(false, true, true, false), StageAction::SummarizeDeferred);
    }

    #[test]
    fn neither_marker_is_done() {
        assert_eq!(decide_stage(false, false, true, true), StageAction::Done);
        assert_eq!(decide_stage(false, false, false, false), StageAction::Done);
    }
}
