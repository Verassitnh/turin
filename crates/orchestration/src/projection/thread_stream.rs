//! Port of upstream/apps/server/src/orchestration-v2/ThreadStream.ts
//!
//! The resume decision and replay budgets only. `buildBoundedThreadStreamSnapshot`
//! and `boundedSnapshotResponseFields` belong to the cut bounded snapshot /
//! wire projection transport.

use serde::Serialize;

/// Maximum number of reducer applications allowed during a thread resume.
pub const THREAD_RESUME_MAX_REPLAY_EVENTS: u64 = 128;

/// Maximum encoded event JSON allowed during a thread resume.
pub const THREAD_RESUME_MAX_REPLAY_ENCODED_BYTES: u64 = 1_048_576;

/// Maximum aggregate persisted payload JSON decoded for one thread resume.
/// Keep the original preflight budget: decoded objects and temporary strings
/// cost more than their stored JSON, even if wire projection shrinks them later.
pub const THREAD_RESUME_MAX_RAW_PAYLOAD_BYTES: u64 = 1_048_576;

/// Resource-safety check applied before persisted replay payloads are decoded.
pub fn is_thread_replay_raw_payload_safe(raw_payload_bytes: u64) -> bool {
    raw_payload_bytes <= THREAD_RESUME_MAX_RAW_PAYLOAD_BYTES
}

/// Encoded payload cost after events have been projected for the wire.
pub fn thread_replay_encoded_bytes<T: Serialize>(items: &[T]) -> u64 {
    items
        .iter()
        .map(|item| serde_json::to_vec(item).map_or(0, |encoded| encoded.len() as u64))
        .sum()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadResumePlan {
    Replay {
        after_sequence: u64,
        through_sequence: u64,
    },
    Snapshot,
}

/// Decide whether a thread subscription should replay the event gap after the
/// client's cursor or send a fresh snapshot instead.
///
/// A client cursor above the high water mark is stale or invalid. Event count
/// limits reducer churn while encoded bytes limit a small number of large
/// updates. Either excess is cheaper to replace with one current snapshot.
pub fn decide_thread_resume(
    after_sequence: u64,
    high_water: u64,
    replay_event_count: u64,
    replay_encoded_bytes: u64,
) -> ThreadResumePlan {
    if after_sequence > high_water
        || replay_event_count > THREAD_RESUME_MAX_REPLAY_EVENTS
        || replay_encoded_bytes > THREAD_RESUME_MAX_REPLAY_ENCODED_BYTES
    {
        return ThreadResumePlan::Snapshot;
    }
    ThreadResumePlan::Replay {
        after_sequence,
        through_sequence: high_water,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replays_when_the_gap_is_zero() {
        assert_eq!(
            decide_thread_resume(10, 10, 0, 0),
            ThreadResumePlan::Replay {
                after_sequence: 10,
                through_sequence: 10
            }
        );
    }

    #[test]
    fn replays_when_the_event_count_is_within_the_bound() {
        assert_eq!(
            decide_thread_resume(
                10,
                20_000,
                THREAD_RESUME_MAX_REPLAY_EVENTS,
                THREAD_RESUME_MAX_REPLAY_ENCODED_BYTES
            ),
            ThreadResumePlan::Replay {
                after_sequence: 10,
                through_sequence: 20_000
            }
        );
    }

    #[test]
    fn falls_back_to_a_snapshot_when_the_event_count_exceeds_the_bound() {
        assert_eq!(
            decide_thread_resume(10, 20_000, THREAD_RESUME_MAX_REPLAY_EVENTS + 1, 1),
            ThreadResumePlan::Snapshot
        );
    }

    #[test]
    fn falls_back_to_a_snapshot_when_encoded_replay_bytes_exceed_the_bound() {
        assert_eq!(
            decide_thread_resume(10, 11, 1, THREAD_RESUME_MAX_REPLAY_ENCODED_BYTES + 1),
            ThreadResumePlan::Snapshot
        );
    }

    #[test]
    fn rejects_pathological_raw_payloads_before_decoding_them() {
        assert!(is_thread_replay_raw_payload_safe(
            THREAD_RESUME_MAX_RAW_PAYLOAD_BYTES
        ));
        assert!(!is_thread_replay_raw_payload_safe(
            THREAD_RESUME_MAX_RAW_PAYLOAD_BYTES + 1
        ));
        // Raw safety and projected transport size are deliberately separate.
        assert_eq!(
            decide_thread_resume(9, 10, 1, 1),
            ThreadResumePlan::Replay {
                after_sequence: 9,
                through_sequence: 10
            }
        );
    }

    #[test]
    fn rejects_a_ten_mib_raw_replay_even_when_its_projected_form_fits() {
        // Wire projection is cut, so the projected size is stood in by a small
        // number. The raw gate must still reject before decoding.
        const TEN_MIB: u64 = 10 * 1024 * 1024;
        assert!(!is_thread_replay_raw_payload_safe(TEN_MIB));
        assert_eq!(
            decide_thread_resume(9, 10, 1, 512),
            ThreadResumePlan::Replay {
                after_sequence: 9,
                through_sequence: 10
            }
        );
    }

    #[test]
    fn falls_back_to_a_snapshot_when_the_client_cursor_is_ahead_of_the_store() {
        assert_eq!(
            decide_thread_resume(50, 40, 0, 0),
            ThreadResumePlan::Snapshot
        );
    }

    #[test]
    fn counts_utf8_bytes_across_projected_stream_items() {
        assert_eq!(
            thread_replay_encoded_bytes(&[json!({"value": "a"}), json!({"value": "🦊"})]),
            (r#"{"value":"a"}"#.len() + r#"{"value":"🦊"}"#.len()) as u64
        );
    }
}
