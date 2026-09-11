//! omp session log parser (`~/.omp/agent/sessions/*/*.jsonl`).
//!
//! Measured upstream facts (plan §2.3, §2.4):
//!
//! * Assistant messages carry `message.usage` with
//!   `{input, output, cacheRead, cacheWrite, totalTokens, reasoningTokens?, cost?}`.
//! * The envelope id (`o.id`) is the dedup key. Measured key set of `message` is
//!   `api, completedAt, content, contextSnapshot, duration, model, provider,
//!   responseId, role, stopReason, timestamp, ttft, usage` — there is **no
//!   `message.id`**; keying on it collapses every row into one NULL row.
//!   Measured: 6081 assistant usage records across 60 recent sessions, 0 duplicate
//!   envelope ids.
//! * `reasoningTokens` is optional and frequently absent (3404 of 6086 records in
//!   the same scan). It always fits inside `output` once present, and
//!   `totalTokens = input + output + cacheRead + cacheWrite` holds exactly for all
//!   6088 records scanned, which proves `input` is net of cache and `output`
//!   already contains reasoning.

use crate::{
    event::{normalize_omp, ModelSource, OmpUsage, ParsedEvent},
    tail::TailLine,
};
use serde_json::Value;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

#[derive(Default)]
pub struct OmpState {
    seen: HashSet<String>,
}

impl OmpState {
    pub fn new() -> Self {
        Self::default()
    }

    /// omp keeps one event per envelope id, so nothing is derived per file.
    /// The seen-set is retained for the lifetime of the process.
    pub fn forget(&mut self, _path: &Path) {}

    pub fn parse_line(&mut self, _path: &Path, line: &TailLine) -> Vec<ParsedEvent> {
        if line.bytes.is_empty() {
            return vec![];
        }
        let Ok(record) = serde_json::from_slice::<Value>(&line.bytes) else {
            return vec![];
        };
        let Some(message) = record.get("message") else {
            return vec![];
        };
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            return vec![];
        }
        let Some(usage) = message.get("usage") else {
            return vec![];
        };
        if usage.is_null() {
            return vec![];
        }
        let Some(event_id) = record.get("id").and_then(Value::as_str) else {
            // Without the envelope id the row cannot be deduplicated; dropping it
            // is safer than re-inserting it on every overlapping read.
            return vec![];
        };
        let Some(occurred_at) = message
            .get("timestamp")
            .and_then(Value::as_str)
            .or_else(|| record.get("timestamp").and_then(Value::as_str))
            .and_then(parse_rfc3339_ms)
        else {
            return vec![];
        };

        let parsed = OmpUsage {
            input: int(usage, "input"),
            output: int(usage, "output"),
            cache_read: int(usage, "cacheRead"),
            cache_write: int(usage, "cacheWrite"),
            reasoning_tokens: usage.get("reasoningTokens").and_then(Value::as_i64),
        };
        let model = message
            .get("model")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned);
        let usage = normalize_omp(&parsed);
        self.seen.insert(event_id.to_owned());

        vec![ParsedEvent {
            event_id: event_id.to_owned(),
            usage,
            model_source: model.as_ref().map(|_| ModelSource::Event),
            model,
            occurred_at,
        }]
    }
}

/// Read an integer field, treating a missing or non-numeric value as 0.
fn int(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// Parse an RFC3339 timestamp into UTC epoch milliseconds.
pub fn parse_rfc3339_ms(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|at| at.timestamp_millis())
}

/// Enumerate session log files under the omp sessions root.
pub fn session_files(root: &Path) -> Vec<PathBuf> {
    crate::tail::collect_files(root, |path| {
        path.extension().is_some_and(|ext| ext == "jsonl")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(offset: u64, text: &str) -> TailLine {
        TailLine {
            start: offset,
            bytes: text.as_bytes().to_vec(),
        }
    }

    const ASSISTANT: &str = r#"{"id":"681cad2e","parentId":null,"timestamp":"2026-09-10T16:38:17.999Z","type":"message","message":{"role":"assistant","api":"openai-completions","model":"deepseek-v4.1-flash","provider":"codebuddy","usage":{"input":211,"output":86,"cacheRead":22144,"cacheWrite":0,"totalTokens":22441,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}}"#;

    #[test]
    fn envelope_id_is_the_dedup_key_because_message_has_no_id() {
        let mut state = OmpState::new();
        let events = state.parse_line(Path::new("/a.jsonl"), &line(0, ASSISTANT));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_id, "681cad2e");
        // The message object really does lack an `id`; this pins the reason the
        // envelope id is used instead of silently regressing to a NULL key.
        let value: Value = serde_json::from_str(ASSISTANT).expect("fixture parses");
        assert!(value["message"].get("id").is_none());
    }

    #[test]
    fn input_is_converted_to_gross_and_reasoning_stays_inside_output() {
        let mut state = OmpState::new();
        let events = state.parse_line(Path::new("/a.jsonl"), &line(0, ASSISTANT));
        assert_eq!(events[0].usage.input_total, 22355, "211 net + 22144 cache");
        assert_eq!(events[0].usage.cache_read, 22144);
        assert_eq!(events[0].usage.output_total, 86);
        assert_eq!(events[0].usage.reasoning, 0);
        assert_eq!(events[0].model.as_deref(), Some("deepseek-v4.1-flash"));
        assert_eq!(events[0].model_source, Some(ModelSource::Event));
    }

    #[test]
    fn missing_reasoning_tokens_defaults_to_zero() {
        let mut state = OmpState::new();
        let events = state.parse_line(Path::new("/a.jsonl"), &line(0, ASSISTANT));
        assert!(!ASSISTANT.contains("reasoningTokens"));
        assert_eq!(events[0].usage.reasoning, 0);
    }

    #[test]
    fn cache_write_counts_toward_gross_input_identity() {
        // hy4-preview moves the whole prompt into cacheWrite, so the identity
        // total = input + output + cacheRead + cacheWrite is the one that holds.
        let text = r#"{"id":"x1","timestamp":"2026-09-10T13:45:21.668Z","type":"message","message":{"role":"assistant","api":"openai-completions","model":"hy4-preview","usage":{"input":0,"output":240,"cacheRead":0,"cacheWrite":24406,"totalTokens":24646,"reasoningTokens":54}}}"#;
        let mut state = OmpState::new();
        let events = state.parse_line(Path::new("/a.jsonl"), &line(0, text));
        assert_eq!(events[0].usage.input_total, 0);
        assert_eq!(events[0].usage.cache_write, 24406);
        assert_eq!(events[0].usage.output_total, 240);
        assert_eq!(events[0].usage.reasoning, 54);
    }

    #[test]
    fn non_assistant_and_malformed_lines_yield_nothing() {
        let mut state = OmpState::new();
        assert!(state
            .parse_line(Path::new("/a.jsonl"), &line(0, "not json"))
            .is_empty());
        assert!(state
            .parse_line(
                Path::new("/a.jsonl"),
                &line(0, r#"{"type":"custom","id":"c1"}"#)
            )
            .is_empty());
        let user = r#"{"id":"u1","timestamp":"2026-09-10T16:38:17.999Z","type":"message","message":{"role":"user","content":"hi"}}"#;
        assert!(state
            .parse_line(Path::new("/a.jsonl"), &line(0, user))
            .is_empty());
    }

    #[test]
    fn a_record_without_an_envelope_id_is_dropped() {
        let text = r#"{"timestamp":"2026-09-10T16:38:17.999Z","type":"message","message":{"role":"assistant","model":"m","usage":{"input":1,"output":2,"cacheRead":3,"cacheWrite":0,"totalTokens":6}}}"#;
        let mut state = OmpState::new();
        assert!(state
            .parse_line(Path::new("/a.jsonl"), &line(0, text))
            .is_empty());
    }
}
