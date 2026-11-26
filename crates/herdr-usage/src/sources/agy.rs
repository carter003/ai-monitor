//! Antigravity CLI (`agy` / `agy2`) transcript logs -> [`ParsedEvent`].
//!
//! Reads `<profile>/antigravity-cli/brain/<conversation_id>/.system_generated/logs/transcript.jsonl`
//! strictly read-only without touching OAuth token files or the network.
//!
//! Measured facts (verified against both `~/.gemini` and `~/.gemini2` corpora
//! and cross-checked with `conversations/*.db` `gen_metadata` protobuf blobs):
//!
//! * `transcript.jsonl` is strictly append-only; each step writes one JSONL
//!   line with a unique `step_index`, `type`, `status`, `created_at` (RFC3339),
//!   and—on model inference steps (`PLANNER_RESPONSE`, `CHECKPOINT`)—token
//!   counters `input_tokens`, `cache_read_tokens`, `output_tokens`.
//! * `input_tokens` is net of `cache_read_tokens`; `output_tokens` is gross
//!   (thinking/reasoning tokens are already included).
//! * `PLANNER_RESPONSE` carries `model` with a thinking-effort suffix (e.g.
//!   `gemini-4-neon-high`, `gemini-3.8-flash-low`, `claude-opus-5-5-high`);
//!   `CHECKPOINT` omits `model` and inherits the conversation's most recent
//!   model as `ModelSource::Context`.
//! * `transcript_full.jsonl` sits alongside `transcript.jsonl` and duplicates
//!   the same steps; only `transcript.jsonl` is collected.

use crate::{
    event::{normalize_agy, AgyUsage, ModelSource, ParsedEvent},
    tail::TailLine,
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

/// Upper bound for the one-time head scan to recover the latest model after a
/// collector restart when a `CHECKPOINT` arrives before any new `PLANNER_RESPONSE`.
const RECOVER_SCAN_LIMIT: u64 = 16 * 1024 * 1024;

/// Which Antigravity CLI profile a transcript belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgyProfile {
    pub account_key: &'static str,
    pub account_label: &'static str,
}

pub const AGY_PRIMARY: AgyProfile = AgyProfile {
    account_key: "agy",
    account_label: "AGY",
};

pub const AGY_SECONDARY: AgyProfile = AgyProfile {
    account_key: "agy2",
    account_label: "AGY2",
};

/// Enumerate `<brain_root>/<conversation_id>/.system_generated/logs/transcript.jsonl`.
///
/// Only the direct children of `brain_root` are inspected so we never walk
/// large artifact or chunk subtrees, and `transcript_full.jsonl` is never read.
pub fn transcript_files(brain_root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(brain_root) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| {
            entry
                .path()
                .join(".system_generated/logs/transcript.jsonl")
        })
        .filter(|path| path.is_file())
        .collect();
    files.sort();
    files
}

/// Extract `<conversation_id>` from
/// `.../brain/<conversation_id>/.system_generated/logs/transcript.jsonl`.
pub fn conversation_id(path: &Path) -> Option<String> {
    path.parent()? // logs
        .parent()? // .system_generated
        .parent()? // <conversation_id>
        .file_name()?
        .to_str()
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
}

/// Normalize Antigravity CLI model slugs so they match OpenRouter `model_price`
/// bare names and merge cleanly with other clients in rankings.
///
/// Examples:
/// * `gemini-4-neon-high` -> `gemini-4-neon`
/// * `gemini-3.8-flash-medium` -> `gemini-3.8-flash`
/// * `claude-opus-5-5-high` -> `claude-opus-5.5`
/// * `claude-sonnet-5-low` -> `claude-sonnet-5`
pub fn normalize_model(raw: &str) -> String {
    let trimmed = raw.trim();
    let stripped = strip_effort_suffix(trimmed);
    normalize_claude_version(stripped)
}

fn strip_effort_suffix(model: &str) -> &str {
    // OpenAI reasoning-model names like `o3-mini-high` / `o4-mini-high` are
    // real distinct price IDs, not effort-suffixed Antigravity slugs.
    if model.ends_with("-mini-high") {
        return model;
    }
    for suffix in ["-xhigh", "-high", "-medium", "-low", "-minimal"] {
        if let Some(base) = model.strip_suffix(suffix) {
            if !base.is_empty() {
                return base;
            }
        }
    }
    model
}

/// Convert `claude-<family>-<major>-<minor>` (e.g. `claude-opus-5-5`) into
/// `claude-<family>-<major>.<minor>` (`claude-opus-5.5`).
fn normalize_claude_version(model: &str) -> String {
    if !model.starts_with("claude-") {
        return model.to_owned();
    }
    let Some((prefix, minor)) = model.rsplit_once('-') else {
        return model.to_owned();
    };
    let Some((head, major)) = prefix.rsplit_once('-') else {
        return model.to_owned();
    };
    let is_short_digits =
        |part: &str| !part.is_empty() && part.len() <= 2 && part.bytes().all(|b| b.is_ascii_digit());
    if is_short_digits(major) && is_short_digits(minor) {
        format!("{head}-{major}.{minor}")
    } else {
        model.to_owned()
    }
}

/// Per-file tracked model state across incremental reads.
#[derive(Debug, Default)]
pub struct AgyState {
    models: HashMap<PathBuf, String>,
    scanned_heads: HashSet<PathBuf>,
}

impl AgyState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn forget(&mut self, path: &Path) {
        self.models.remove(path);
        self.scanned_heads.remove(path);
    }

    pub fn model_for(&self, path: &Path) -> Option<&str> {
        self.models.get(path).map(String::as_str)
    }

    fn recover_model_from_head(&mut self, path: &Path, before: u64) {
        let to = before.min(RECOVER_SCAN_LIMIT);
        if to == 0 {
            return;
        }
        let Ok(head_bytes) = crate::tail::read_range(path, 0, to) else {
            return;
        };
        self.scanned_heads.insert(path.to_path_buf());
        if let Some(model) = extract_latest_model(&head_bytes) {
            self.models.insert(path.to_path_buf(), model);
        }
    }

    /// Parse one fresh line from a `transcript.jsonl` file.
    pub fn parse_line(
        &mut self,
        path: &Path,
        line: &TailLine,
        profile: AgyProfile,
    ) -> Vec<ParsedEvent> {
        if !contains(&line.bytes, b"\"input_tokens\"")
            && !contains(&line.bytes, b"\"output_tokens\"")
            && !contains(&line.bytes, b"\"cache_read_tokens\"")
        {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&line.bytes) else {
            return Vec::new();
        };

        // Ignore non-DONE records if upstream ever emits streaming snapshots.
        if let Some(status) = value.get("status").and_then(serde_json::Value::as_str) {
            if status != "DONE" {
                return Vec::new();
            }
        }

        let raw_usage = AgyUsage {
            input_tokens: int_at(&value, "input_tokens"),
            cache_read_tokens: int_at(&value, "cache_read_tokens"),
            cache_write_tokens: int_at(&value, "cache_write_tokens"),
            output_tokens: int_at(&value, "output_tokens"),
            thinking_tokens: int_at(&value, "thinking_tokens"),
        };

        let explicit_model = value
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(normalize_model);
        if let Some(ref model) = explicit_model {
            self.models.insert(path.to_path_buf(), model.clone());
        }

        if raw_usage.input_tokens <= 0
            && raw_usage.cache_read_tokens <= 0
            && raw_usage.cache_write_tokens <= 0
            && raw_usage.output_tokens <= 0
        {
            return Vec::new();
        }

        let Some(occurred_at) = value
            .get("created_at")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_rfc3339_millis)
        else {
            return Vec::new();
        };

        let (model, model_source) = match explicit_model {
            Some(model) => (Some(model), Some(ModelSource::Event)),
            None => {
                if !self.models.contains_key(path) && !self.scanned_heads.contains(path) {
                    self.recover_model_from_head(path, line.start);
                }
                match self.model_for(path) {
                    Some(model) => (Some(model.to_owned()), Some(ModelSource::Context)),
                    None => (None, None),
                }
            }
        };

        let event_id = match value.get("step_index").and_then(serde_json::Value::as_i64) {
            Some(step_index) => format!("{}:step:{step_index}", path.display()),
            None => format!("{}:{}", path.display(), line.start),
        };

        vec![ParsedEvent {
            event_id,
            usage: normalize_agy(&raw_usage),
            model,
            model_source,
            provider: Some("google-antigravity".to_owned()),
            session_id: conversation_id(path),
            started_at: Some(occurred_at),
            completed_at: None,
            duration_ms: None,
            account_key: Some(profile.account_key.to_owned()),
            account_label: Some(profile.account_label.to_owned()),
            account_source: Some("profile".to_owned()),
            account_route_reason: None,
            occurred_at,
        }]
    }
}

fn extract_latest_model(bytes: &[u8]) -> Option<String> {
    let mut latest = None;
    for line in bytes.split(|byte| *byte == b'\n') {
        if !contains(line, b"\"model\"") {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(model) = value
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            latest = Some(normalize_model(model));
        }
    }
    latest
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn int_at(value: &serde_json::Value, key: &str) -> i64 {
    value
        .get(key)
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0)
}

fn parse_rfc3339_millis(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|stamp| stamp.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLANNER_TURN_1: &[u8] = br#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-11-26T14:41:52Z","model":"gemini-4-neon-high","input_tokens":11680,"cache_read_tokens":0,"output_tokens":524}"#;
    const PLANNER_TURN_2: &[u8] = br#"{"step_index":5,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-11-26T14:42:09Z","model":"gemini-4-neon-high","input_tokens":8766,"cache_read_tokens":11681,"output_tokens":466}"#;
    const CHECKPOINT: &[u8] = br#"{"step_index":237,"source":"SYSTEM","type":"CHECKPOINT","status":"DONE","created_at":"2026-11-26T03:55:47Z","input_tokens":6192,"cache_read_tokens":511795,"output_tokens":5467,"truncated_fields":["content"]}"#;
    const CLAUDE_TURN: &[u8] = br#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-11-25T07:44:34Z","model":"claude-opus-5-5-high","input_tokens":15469,"cache_read_tokens":8519,"output_tokens":282}"#;

    fn transcript_path(home: &str, cid: &str) -> PathBuf {
        PathBuf::from(format!(
            "{home}/antigravity-cli/brain/{cid}/.system_generated/logs/transcript.jsonl"
        ))
    }

    fn tail_line(bytes: &[u8], start: u64) -> TailLine {
        TailLine {
            start,
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn normalizes_effort_suffixes_and_claude_versions() {
        assert_eq!(normalize_model("gemini-4-neon-high"), "gemini-4-neon");
        assert_eq!(normalize_model("gemini-4-hydrogen-xhigh"), "gemini-4-hydrogen");
        assert_eq!(normalize_model("gemini-3.8-flash-medium"), "gemini-3.8-flash");
        assert_eq!(normalize_model("gemini-3.8-flash-low"), "gemini-3.8-flash");
        assert_eq!(normalize_model("gemini-4-carbon-minimal"), "gemini-4-carbon");
        assert_eq!(normalize_model("claude-opus-5-5-high"), "claude-opus-5.5");
        assert_eq!(normalize_model("claude-opus-5-5-medium"), "claude-opus-5.5");
        assert_eq!(normalize_model("claude-sonnet-5-high"), "claude-sonnet-5");
        assert_eq!(normalize_model("o3-mini-high"), "o3-mini-high");
    }

    #[test]
    fn adds_cache_read_into_gross_input_and_tags_profile() {
        let mut state = AgyState::new();
        let path = transcript_path("/home/u/.gemini2", "6c0c3637-68e5-4469-856b-cc9121dd7e04");
        let events = state.parse_line(&path, &tail_line(PLANNER_TURN_2, 14359), AGY_SECONDARY);
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.usage.input_total, 8766 + 11681);
        assert_eq!(event.usage.cache_read, 11681);
        assert_eq!(event.usage.cache_write, 0);
        assert_eq!(event.usage.output_total, 466);
        assert_eq!(event.usage.reasoning, 0);
        assert_eq!(event.model.as_deref(), Some("gemini-4-neon"));
        assert_eq!(event.model_source, Some(ModelSource::Event));
        assert_eq!(event.provider.as_deref(), Some("google-antigravity"));
        assert_eq!(
            event.session_id.as_deref(),
            Some("6c0c3637-68e5-4469-856b-cc9121dd7e04")
        );
        assert_eq!(event.account_key.as_deref(), Some("agy2"));
        assert_eq!(event.account_label.as_deref(), Some("AGY2"));
        assert_eq!(event.account_source.as_deref(), Some("profile"));
        assert_eq!(event.event_id, format!("{}:step:5", path.display()));
    }

    #[test]
    fn checkpoint_inherits_most_recent_model_in_conversation() {
        let mut state = AgyState::new();
        let path = transcript_path("/home/u/.gemini", "89e981b9-4210-402c-87a1-10ce5fa6961c");
        assert_eq!(
            state.parse_line(&path, &tail_line(PLANNER_TURN_1, 0), AGY_PRIMARY).len(),
            1
        );
        let events = state.parse_line(&path, &tail_line(CHECKPOINT, 500), AGY_PRIMARY);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].model.as_deref(), Some("gemini-4-neon"));
        assert_eq!(events[0].model_source, Some(ModelSource::Context));
        assert_eq!(events[0].usage.input_total, 6192 + 511795);
        assert_eq!(events[0].usage.cache_read, 511795);
        assert_eq!(events[0].usage.output_total, 5467);
        assert_eq!(events[0].account_key.as_deref(), Some("agy"));
        assert_eq!(events[0].account_label.as_deref(), Some("AGY"));
    }

    #[test]
    fn claude_turn_maps_to_dotted_version() {
        let mut state = AgyState::new();
        let path = transcript_path("/home/u/.gemini2", "2b029e5c-9236-49a1-baf1-f9163e304e47");
        let events = state.parse_line(&path, &tail_line(CLAUDE_TURN, 0), AGY_SECONDARY);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].model.as_deref(), Some("claude-opus-5.5"));
        assert_eq!(events[0].usage.input_total, 15469 + 8519);
        assert_eq!(events[0].usage.cache_read, 8519);
        assert_eq!(events[0].usage.output_total, 282);
    }

    #[test]
    fn skips_non_token_zero_token_and_non_done_lines() {
        let mut state = AgyState::new();
        let path = transcript_path("/home/u/.gemini", "cid-1");
        let user_line = br#"{"step_index":0,"source":"USER","type":"USER_INPUT","status":"DONE","created_at":"2026-11-26T14:41:52Z","content":"hi"}"#;
        let zero_line = br#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-11-26T14:41:52Z","model":"gemini-4-neon-high","input_tokens":0,"cache_read_tokens":0,"output_tokens":0}"#;
        let running_line = br#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"RUNNING","created_at":"2026-11-26T14:41:52Z","model":"gemini-4-neon-high","input_tokens":100,"cache_read_tokens":0,"output_tokens":10}"#;
        assert!(state.parse_line(&path, &tail_line(user_line, 0), AGY_PRIMARY).is_empty());
        assert!(state.parse_line(&path, &tail_line(zero_line, 100), AGY_PRIMARY).is_empty());
        assert!(state.parse_line(&path, &tail_line(running_line, 200), AGY_PRIMARY).is_empty());
    }
}
