//! codex rollout (`~/.codex/sessions/**/rollout-*.jsonl`) -> [`ParsedEvent`].
//!
//! Measured facts this module depends on (scans of the local corpus, plus a
//! full pass over `rollout-2026-09-10T18-42-07-*.jsonl`, 10 MB / 7066 lines):
//!
//! * The envelope keys are exactly `["payload", "timestamp", "type"]` (newer
//!   files also carry `ordinal`). There is **no** per-line id, so the dedup key
//!   is the absolute file path plus the byte offset of the line start. 554 of
//!   31158 timestamps across 600 files are duplicated, which rules out a
//!   timestamp key; `(file, offset)` is unique and resumable.
//! * Only `payload.type == "token_count"` is collected. A second envelope
//!   (`type == "token_usage_record"`, usage at `payload.usage`) reports the
//!   *same* per-inference counters: over every 2026-09 rollout (31661 records)
//!   all but 4 objects reappear verbatim as a later `token_count`
//!   `last_token_usage`, and each of those 4 is immediately followed by a
//!   `token_count` reporting all-zero tokens, the emit that measures nothing.
//!   Collecting both envelopes would double-count the inference, so
//!   `token_usage_record` is ignored explicitly.
//! * `payload.info` can be `null` (6 of 21325 `token_count` events in a
//!   400-file scan; commonly the first of a session). `info.last_token_usage`
//!   can likewise be absent. Either case skips the line.
//! * `last_token_usage` is the per-inference delta and is what gets summed.
//!   `total_token_usage` is the session cumulative value and must never be
//!   used. Upstream also emits deltas of all zeros (measured 18 per 2026-09-10
//!   file, always right after a compaction/extrapolation step) whose
//!   `last_token_usage.total_tokens` holds a context-window number
//!   (`13006`/`19077`) rather than a token count, which is a second reason to
//!   touch no `total_*` field. Those rows normalize to zero and are left for
//!   the caller to skip.
//! * `token_count` carries no model. The model is the `payload.model` of the
//!   most recent preceding `turn_context` **in the same file**, so it is
//!   tracked per path and the tracking must be dropped when the file rotates.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Substring test used to skip a line before parsing it as JSON.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

use crate::event::{normalize_codex, CodexLastUsage, ModelSource, ParsedEvent};
use crate::tail::TailLine;

/// Per-file derived state that must survive across incremental read passes.
#[derive(Debug, Default)]
pub struct CodexState {
    /// Model of the most recent `turn_context` seen in each file.
    models: HashMap<PathBuf, String>,
}

impl CodexState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop the tracked `turn_context` model for one file.
    ///
    /// MUST be called by the collector when a file is rotated or truncated:
    /// offsets restart at zero, and the model recorded before the rotation
    /// describes a different session.
    pub fn forget(&mut self, path: &Path) {
        self.models.remove(path);
    }

    /// Recover the tracked model from a range of the file that will not be
    /// collected as events.
    ///
    /// The first round baselines a file at its current size and reads nothing,
    /// and a restart on a large rollout starts at the persisted watermark. Both
    /// cases would otherwise leave every later `token_count` without a model, so
    /// the caller replays just the `turn_context` lines from the skipped prefix:
    /// only the last one matters, and the scan is bounded by `limit`.
    pub fn seed_model(&mut self, path: &Path, bytes: &[u8]) {
        if let Some(model) = extract_latest_model(bytes) {
            self.models.insert(path.to_path_buf(), model);
        } else if !self.models.contains_key(path) {
            self.recover_model_from_head(path);
        }
    }

    /// Scan the initial 64 KB of `path` for a `turn_context` line.
    ///
    /// When a rollout file is larger than the 1 MB tail checked during baselining,
    fn recover_model_from_head(&mut self, path: &Path) {
        let size = match std::fs::metadata(path) {
            Ok(meta) => meta.len(),
            Err(_) => return,
        };
        let to = size.min(64 * 1024);
        if to == 0 {
            return;
        }
        if let Ok(head_bytes) = crate::tail::read_range(path, 0, to) {
            if let Some(model) = extract_latest_model(&head_bytes) {
                self.models.insert(path.to_path_buf(), model);
            }
        }
    }
    /// Parse one fresh line.
    ///
    /// `line.start` is the byte offset of the line inside the file and is part
    /// of the dedup key. Returns every usage event the line yielded (0, 1 or
    /// more); lines that only update derived state return an empty vector but
    /// still record the state.
    pub fn parse_line(&mut self, path: &Path, line: &TailLine) -> Vec<ParsedEvent> {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&line.bytes) else {
            return Vec::new();
        };
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("turn_context") => {
                self.record_model(path, &value);
                Vec::new()
            }
            // Duplicate of `token_count`, ignored so the inference is counted
            // once (see the module doc).
            Some("token_usage_record") => Vec::new(),
            Some("event_msg") => self.parse_event_msg(path, line, &value),
            _ => Vec::new(),
        }
    }

    /// The model currently tracked for `path`, if a `turn_context` was seen.
    pub fn model_for(&self, path: &Path) -> Option<&str> {
        self.models.get(path).map(String::as_str)
    }

    /// Remember `payload.model`, leaving the previous value in place if the
    /// field is absent or is not a string (schema drift must not erase a known
    /// good model).
    fn record_model(&mut self, path: &Path, value: &serde_json::Value) {
        let model = value
            .pointer("/payload/model")
            .and_then(serde_json::Value::as_str);
        if let Some(model) = model {
            self.models.insert(path.to_path_buf(), model.to_string());
        }
    }

    /// Handle an `event_msg`: act only on `token_count`, and only when it
    /// carries a usable `last_token_usage` and a parseable timestamp.
    fn parse_event_msg(
        &mut self,
        path: &Path,
        line: &TailLine,
        value: &serde_json::Value,
    ) -> Vec<ParsedEvent> {
        if value
            .pointer("/payload/type")
            .and_then(serde_json::Value::as_str)
            != Some("token_count")
        {
            return Vec::new();
        }
        let Some(usage) = value.pointer("/payload/info/last_token_usage") else {
            return Vec::new();
        };
        if usage.is_null() {
            return Vec::new();
        }
        let occurred_at = match value
            .get("timestamp")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_rfc3339_millis)
        {
            Some(millis) => millis,
            None => return Vec::new(),
        };

        let last = CodexLastUsage {
            input_tokens: int_at(usage, "input_tokens"),
            cached_input_tokens: int_at(usage, "cached_input_tokens"),
            // Only present when the provider bills cache writes; measured 0 in
            // every local sample, where the field is present but zero.
            cache_write_input_tokens: int_at(usage, "cache_write_input_tokens"),
            output_tokens: int_at(usage, "output_tokens"),
            reasoning_output_tokens: int_at(usage, "reasoning_output_tokens"),
        };

        if !self.models.contains_key(path) {
            self.recover_model_from_head(path);
        }
        let (model, model_source) = match self.model_for(path) {
            Some(model) => (Some(model.to_string()), Some(ModelSource::Context)),
            None => (None, None),
        };
        vec![ParsedEvent {
            event_id: format!("{}:{}", path.display(), line.start),
            usage: normalize_codex(&last),
            model,
            model_source,
            occurred_at,
        }]
    }
}
/// Extract the latest model from all `turn_context` lines in `bytes`.
fn extract_latest_model(bytes: &[u8]) -> Option<String> {
    let mut latest: Option<String> = None;
    for line in bytes.split(|byte| *byte == b'\n') {
        // Cheap reject before paying for a JSON parse: a `turn_context` line
        // is rare, so this keeps the scan proportional to the number of
        // context switches rather than to the number of tool calls.
        if !contains(line, b"turn_context") {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(model) = value
            .pointer("/payload/model")
            .and_then(serde_json::Value::as_str)
            .filter(|model| !model.is_empty())
        {
            latest = Some(model.to_owned());
        }
    }
    latest
}

/// Read an integer field, degrading to 0 when absent or not an integer.
fn int_at(value: &serde_json::Value, key: &str) -> i64 {
    value
        .get(key)
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0)
}

/// RFC3339 (e.g. `2026-05-28T09:12:33.123Z`) to UTC epoch milliseconds.
///
/// Hand-parsing is deliberately avoided: `chrono` already covers the fractional
/// seconds and any offset form upstream may emit.
fn parse_rfc3339_millis(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|stamp| stamp.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `token_count` line, measured 2026-05-28T09:12:33.123Z. Note
    /// `total_token_usage` is the session cumulative value (29511) while
    /// `last_token_usage` is this inference's delta (16224).
    const TOKEN_COUNT: &[u8] = br#"{"timestamp":"2026-05-28T09:12:33.123Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":29027,"cached_input_tokens":17152,"output_tokens":484,"reasoning_output_tokens":69,"total_tokens":29511},"last_token_usage":{"input_tokens":16067,"cached_input_tokens":12672,"output_tokens":157,"reasoning_output_tokens":0,"total_tokens":16224},"model_context_window":258400},"rate_limits":{"limit_id":"codex"}}}"#;

    /// Real `turn_context` line (payload trimmed, keys kept verbatim).
    const TURN_CONTEXT: &[u8] = br#"{"timestamp":"2026-05-28T09:12:30.000Z","type":"turn_context","payload":{"turn_id":"019e6dcb-0000-7000-8000-000000000000","cwd":"/home/carter003/project/erp-1123","current_date":"2026-05-28","timezone":"Asia/Shanghai","model":"gpt-5.5","personality":"pragmatic"}}"#;

    /// Real `token_count` whose `info` is null (measured 6 of 21325, and the
    /// first `token_count` of a session is the common case). `ordinal` and the
    /// `rate_limits` object are dropped here to keep the line short; the
    /// `info: null` shape is verbatim.
    const TOKEN_COUNT_NULL_INFO: &[u8] = br#"{"timestamp":"2026-09-08T01:29:12.271Z","ordinal":12,"type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"limit_id":"codex","plan_type":"pro"}}}"#;

    /// Real `token_usage_record` line (measured verbatim
    /// `rollout-2026-09-04T10-06-59-*.jsonl` line 20; trailing
    /// `thread_token_usage` dropped for brevity).
    const TOKEN_USAGE_RECORD: &[u8] = br#"{"timestamp":"2026-09-04T02:07:16.436Z","ordinal":19,"type":"token_usage_record","payload":{"thread_id":"01a06a2b-53cc-7c00-adda-d4651a81eeca","turn_id":"01a06a2b-85f7-7cd0-ac97-a69f7d3609ec","session_id":"01a06a2b-53cc-7c00-adda-d4651a81eeca","root_turn_id":"01a06a2b-85f7-7cd0-ac97-a69f7d3609ec","response_id":"resp_082b499a941ff2a0016a9a27d2c36487d08f9f9017bdba4945","usage":{"input_tokens":17936,"cached_input_tokens":4608,"cache_write_input_tokens":0,"output_tokens":552,"reasoning_output_tokens":415,"total_tokens":18488},"turn_token_usage":{"input_tokens":17936,"cached_input_tokens":4608,"cache_write_input_tokens":0,"output_tokens":552,"reasoning_output_tokens":415,"total_tokens":18488}}}"#;

    fn file() -> PathBuf {
        PathBuf::from(
            "/home/carter003/.codex/sessions/2026/09/10/rollout-2026-09-10T18-42-07.jsonl",
        )
    }

    fn line(bytes: &[u8], start: u64) -> TailLine {
        TailLine {
            start,
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn null_info_yields_nothing_instead_of_panicking() {
        let mut state = CodexState::new();
        let events = state.parse_line(&file(), &line(TOKEN_COUNT_NULL_INFO, 4096));
        assert!(events.is_empty());
    }

    #[test]
    fn absent_last_token_usage_yields_nothing() {
        let mut state = CodexState::new();
        let bytes = br#"{"timestamp":"2026-05-28T09:12:33.123Z","type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":258400}}}"#;
        assert!(state.parse_line(&file(), &line(bytes, 17)).is_empty());
    }

    #[test]
    fn turn_context_supplies_the_model_for_following_events() {
        let mut state = CodexState::new();
        assert!(state.parse_line(&file(), &line(TURN_CONTEXT, 0)).is_empty());
        assert_eq!(state.model_for(&file()), Some("gpt-5.5"));

        let events = state.parse_line(&file(), &line(TOKEN_COUNT, 512));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].model.as_deref(), Some("gpt-5.5"));
        assert_eq!(events[0].model_source, Some(ModelSource::Context));
    }

    #[test]
    fn events_before_any_turn_context_carry_no_model() {
        let mut state = CodexState::new();
        let events = state.parse_line(&file(), &line(TOKEN_COUNT, 0));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].model, None);
        assert_eq!(events[0].model_source, None);
    }

    #[test]
    fn the_tracked_model_survives_forget_only_for_the_given_file() {
        let other = PathBuf::from("/home/carter003/.codex/sessions/2026/09/11/rollout-b.jsonl");
        let mut state = CodexState::new();
        state.parse_line(&file(), &line(TURN_CONTEXT, 0));
        state.parse_line(&other, &line(TURN_CONTEXT, 0));
        assert_eq!(state.model_for(&other), Some("gpt-5.5"));

        // A fresh file has no context until its own turn_context arrives.
        state.forget(&other);
        assert_eq!(state.model_for(&other), None);
        assert_eq!(
            state.parse_line(&other, &line(TOKEN_COUNT, 0))[0].model,
            None
        );

        // Rotation drops only the rotated path.
        state.forget(&file());
        assert_eq!(state.model_for(&file()), None);
        assert_eq!(
            state.parse_line(&file(), &line(TOKEN_COUNT, 0))[0].model_source,
            None
        );
    }

    #[test]
    fn a_later_turn_context_replaces_the_tracked_model() {
        let mut state = CodexState::new();
        state.parse_line(&file(), &line(TURN_CONTEXT, 0));
        let switched = br#"{"timestamp":"2026-05-28T09:20:00.000Z","type":"turn_context","payload":{"turn_id":"019e6dcb-1111-7000-8000-000000000000","cwd":"/home/carter003/project/erp-1123","model":"gpt-6-astra"}}"#;
        state.parse_line(&file(), &line(switched, 100));

        let events = state.parse_line(&file(), &line(TOKEN_COUNT, 200));
        assert_eq!(events[0].model.as_deref(), Some("gpt-6-astra"));
    }
    #[test]
    fn seed_model_recovers_from_file_head_when_tail_has_no_turn_context() {
        let temp = std::env::temp_dir().join(format!("test-rollout-{}.jsonl", std::process::id()));
        // Write turn_context at the start of the file
        let mut content = TURN_CONTEXT.to_vec();
        content.push(b'\n');
        content.extend_from_slice(b"{\"type\":\"other\"}\n".repeat(100).as_slice());
        std::fs::write(&temp, &content).expect("write");

        let mut state = CodexState::new();
        // Tail bytes passed to seed_model do not contain turn_context
        let tail_bytes = b"{\"type\":\"other\"}\n";
        state.seed_model(&temp, tail_bytes);
        assert_eq!(state.model_for(&temp), Some("gpt-5.5"));

        let _ = std::fs::remove_file(&temp);
    }

    #[test]
    fn token_usage_record_is_ignored_because_it_duplicates_token_count() {
        // Measured: across all 2026-09 rollouts, 31657 of 31661
        // `token_usage_record` usage objects reappear verbatim as a later
        // `token_count` `last_token_usage`; this fixture is one of them
        // (rollout-2026-09-04T10-06-59-*.jsonl line 20 -> the token_count for
        // the same counters follows within the same second). Emitting both
        // envelopes would put the same inference in the database twice, and
        // the 4 counterexamples are each followed by a zero-token
        // `token_count` carrying no usage at all.
        let mut state = CodexState::new();
        state.parse_line(&file(), &line(TURN_CONTEXT, 0));
        let events = state.parse_line(&file(), &line(TOKEN_USAGE_RECORD, 1024));
        assert!(events.is_empty());
        assert_eq!(state.model_for(&file()), Some("gpt-5.5"));
    }

    #[test]
    fn event_id_carries_the_path_and_the_line_offset() {
        let mut state = CodexState::new();
        let first = state.parse_line(&file(), &line(TOKEN_COUNT, 512));
        let second = state.parse_line(&file(), &line(TOKEN_COUNT, 1400));

        assert_eq!(
            first[0].event_id,
            "/home/carter003/.codex/sessions/2026/09/10/rollout-2026-09-10T18-42-07.jsonl:512"
        );
        // Two byte-identical lines are two distinct inferences, so the offset
        // must be what separates them: timestamps repeat (554 of 31158).
        assert_eq!(first[0].event_id, format!("{}:{}", file().display(), 512));
        assert_ne!(first[0].event_id, second[0].event_id);
        assert_eq!(second[0].event_id, format!("{}:{}", file().display(), 1400));
    }

    #[test]
    fn last_token_usage_is_summed_not_total_token_usage() {
        let mut state = CodexState::new();
        let events = state.parse_line(&file(), &line(TOKEN_COUNT, 0));
        let usage = events[0].usage;

        assert_eq!(usage.input_total, 16067); // not 29027
        assert_eq!(usage.cache_read, 12672); // not 17152
        assert_eq!(usage.cache_write, 0);
        assert_eq!(usage.output_total, 157); // not 484
        assert_eq!(usage.reasoning, 0); // not 69
    }

    #[test]
    fn utc_millis_are_read_from_the_rfc3339_timestamp() {
        let mut state = CodexState::new();
        let events = state.parse_line(&file(), &line(TOKEN_COUNT, 0));
        // 2026-05-28T09:12:33.123Z measured with `date -u -d` on this machine.
        assert_eq!(events[0].occurred_at, 1779959553123);
    }

    #[test]
    fn malformed_and_non_usage_lines_yield_nothing() {
        let mut state = CodexState::new();
        assert!(state
            .parse_line(&file(), &line(b"not json at all", 0))
            .is_empty());
        assert!(state
            .parse_line(&file(), &line(br#"{"timestamp":"oops","#.as_slice(), 10))
            .is_empty());
        assert!(state.parse_line(&file(), &line(b"", 20)).is_empty());
        assert!(state
            .parse_line(
                &file(),
                &line(br#"{"timestamp":"2026-05-28T09:12:33.123Z","ordinal":2,"type":"response_item","payload":{"type":"message","role":"developer"}}"#, 30),
            )
            .is_empty());
        // A token_count without a timestamp cannot supply occurred_at, which is
        // NOT NULL, so the line is skipped even though it has usage.
        let undated = br#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":1,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":1,"reasoning_output_tokens":0}}}}"#;
        assert!(state.parse_line(&file(), &line(undated, 40)).is_empty());
    }

    #[test]
    fn a_non_string_model_does_not_erase_the_tracked_one() {
        let mut state = CodexState::new();
        state.parse_line(&file(), &line(TURN_CONTEXT, 0));
        let drift = br#"{"timestamp":"2026-05-28T09:19:00.000Z","type":"turn_context","payload":{"turn_id":"019e6dcb-2222-7000-8000-000000000000","model":null}}"#;
        state.parse_line(&file(), &line(drift, 50));
        assert_eq!(state.model_for(&file()), Some("gpt-5.5"));
    }
}
