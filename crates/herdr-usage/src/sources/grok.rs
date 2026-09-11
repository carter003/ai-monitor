//! grok source parser (`~/.grok/logs/unified.jsonl`).
//!
//! Measured upstream facts this module relies on:
//!
//! * The log is a single file of JSON objects with the envelope
//!   `ts, src, pid, ver, lvl, sid, msg, ctx`. Everything interesting lives in
//!   `ctx`; the top-level `msg` is what selects the record type.
//! * Usage arrives only as `msg == "shell.turn.inference_done"`. Its `ctx` keys
//!   are fixed and contain **no model field** (`loop_index`, `prompt_tokens`,
//!   `cached_prompt_tokens`, `completion_tokens`, `reasoning_tokens`, …), so the
//!   model can never be read alongside the usage.
//! * `msg == "model changed"` carries `ctx.model` but is rare: 4 lines in the
//!   whole 14934-line log, covering 3 distinct `sid`s, while
//!   `shell.turn.inference_done` involves 37 distinct `sid`s. That is why a
//!   second-level fallback to `config.toml [models].default` exists.
//!
//! ## Dedup key
//!
//! `event_id = "{sid}:{loop_index}:{ts}"`, using the **raw** `ts` string lifted
//! verbatim out of the log. Measured on the full log: `sid + loop_index` alone
//! collapses 1190 `inference_done` events into 1092 distinct keys — 98 events
//! (8%) silently lost. Those collisions are genuinely different inferences, not
//! repeated writes: under one duplicated `sid + loop_index` the prompt tokens
//! climb 21013 → 109325 → 238177. Adding `ts` yields 1190/1190 unique keys. The
//! `ts` field is kept as text rather than reserialized from the parsed
//! timestamp, because the raw millisecond string is already unique.
//!
//! ## Model resolution (this module is the sole owner of it for grok)
//!
//! 1. A `sid -> model` map is fed by every `model changed` line seen so far in
//!    file order. A later `inference_done` on that `sid` reports that model with
//!    [`ModelSource::Event`] (the model came from an observed event).
//! 2. Otherwise the injected `[models].default` value is reported with
//!    [`ModelSource::ConfigFallback`].
//! 3. Otherwise `model = None` and `model_source = None`.
//!
//! The config value is *injected*: the collector process reads `config.toml` and
//! calls [`GrokState::set_default_model`] when it changes. This module performs
//! no file I/O and no TOML parsing, and deliberately ignores
//! `[ui].fork_secondary_model`.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use crate::event::{normalize_grok, GrokCtx, ModelSource, ParsedEvent, UsageEvent};
use crate::tail::TailLine;

/// `msg` value of a usage record.
const MSG_INFERENCE_DONE: &str = "shell.turn.inference_done";
/// `msg` value of a model-switch record.
const MSG_MODEL_CHANGED: &str = "model changed";

/// Per-file grok derivation state.
///
/// The single piece of derived state is the `sid -> model` map fed by
/// `model changed` lines; it exists so the answer survives across incremental
/// read passes over the same file.
#[derive(Debug, Default)]
pub struct GrokState {
    /// `sid` -> model, in file read order. Later observations overwrite earlier
    /// ones for the same session.
    sid_models: HashMap<String, String>,
    /// Injected `config.toml [models].default`.
    default_model: Option<String>,
}

impl GrokState {
    /// New state with the injected `[models].default`, or `None` when the caller
    /// has no config value yet.
    pub fn new(default_model: Option<String>) -> Self {
        Self {
            sid_models: HashMap::new(),
            default_model,
        }
    }

    /// Refresh the injected `[models].default`.
    ///
    /// The collector calls this when `config.toml`'s mtime changes; already
    /// resolved events are unaffected because resolution happens per line.
    pub fn set_default_model(&mut self, model: Option<String>) {
        self.default_model = model;
    }

    /// Drop per-file state. `~/.grok/logs/unified.jsonl` is a single file, so
    /// there is no per-path partitioning to preserve: the whole `sid -> model`
    /// map is cleared. MUST be called when the file is rotated or truncated,
    /// because a re-read from offset 0 replays `model changed` lines in the same
    /// order and rebuilds the map.
    pub fn forget(&mut self, _path: &Path) {
        self.sid_models.clear();
    }

    /// Parse one fresh line.
    ///
    /// `line.start` is the byte offset of the line inside the file and is part
    /// of the caller's watermark bookkeeping; grok's identity is already fully
    /// determined by `sid + loop_index + ts`, so the offset is unused here.
    ///
    /// A line that is not JSON, is empty, or carries no usage yields an empty
    /// `Vec`. A `model changed` line yields an empty `Vec` but still records the
    /// session's model.
    pub fn parse_line(&mut self, _path: &Path, line: &TailLine) -> Vec<ParsedEvent> {
        let Ok(root) = serde_json::from_slice::<Value>(line.bytes.as_slice()) else {
            return Vec::new();
        };
        let Some(msg) = root.get("msg").and_then(Value::as_str) else {
            return Vec::new();
        };
        match msg {
            MSG_MODEL_CHANGED => {
                self.record_model_change(&root);
                Vec::new()
            }
            MSG_INFERENCE_DONE => self.parse_inference_done(&root).into_iter().collect(),
            _ => Vec::new(),
        }
    }

    /// Record `sid -> ctx.model` from a `model changed` line.
    ///
    /// A line missing either field is ignored rather than clearing an existing
    /// entry: an unparseable switch says nothing about the session's model.
    fn record_model_change(&mut self, root: &Value) {
        let Some(sid) = root.get("sid").and_then(Value::as_str) else {
            return;
        };
        let Some(model) = pointer_str(root, "/ctx/model") else {
            return;
        };
        self.sid_models.insert(sid.to_owned(), model.to_owned());
    }

    /// Build the usage event for a `shell.turn.inference_done` line.
    fn parse_inference_done(&mut self, root: &Value) -> Option<ParsedEvent> {
        let sid = root.get("sid").and_then(Value::as_str)?;
        // Kept verbatim: it is both the uniqueness component of the key and the
        // source of `occurred_at`.
        let ts_raw = root.get("ts").and_then(Value::as_str)?;
        let occurred_at = parse_rfc3339_millis(ts_raw)?;
        let loop_index = pointer_i64(root, "/ctx/loop_index");

        let ctx = GrokCtx {
            prompt_tokens: pointer_i64(root, "/ctx/prompt_tokens"),
            cached_prompt_tokens: pointer_i64(root, "/ctx/cached_prompt_tokens"),
            completion_tokens: pointer_i64(root, "/ctx/completion_tokens"),
            reasoning_tokens: pointer_i64(root, "/ctx/reasoning_tokens"),
        };
        let usage: UsageEvent = normalize_grok(&ctx);

        let (model, model_source) = match self.sid_models.get(sid) {
            Some(tracked) => (Some(tracked.clone()), Some(ModelSource::Event)),
            None => match self.default_model.as_ref() {
                Some(fallback) => (Some(fallback.clone()), Some(ModelSource::ConfigFallback)),
                None => (None, None),
            },
        };

        Some(ParsedEvent {
            event_id: format!("{sid}:{loop_index}:{ts_raw}"),
            usage,
            model,
            model_source,
            occurred_at,
        })
    }
}

/// `ctx` is absent on some lines, and individual keys are occasionally `null`
/// (measured: `ttft_ms` and `itl_p50_ms` on some inference records). Both cases
/// degrade to the supplied default instead of failing the parse.
fn pointer_i64(root: &Value, pointer: &str) -> i64 {
    root.pointer(pointer).and_then(Value::as_i64).unwrap_or(0)
}

fn pointer_str<'a>(root: &'a Value, pointer: &str) -> Option<&'a str> {
    root.pointer(pointer).and_then(Value::as_str)
}

/// RFC3339 in the log is always UTC with a `Z` suffix and millisecond precision.
/// Fractional seconds shorter than a millisecond (or absent) are still accepted
/// because chrono truncates toward zero.
fn parse_rfc3339_millis(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|stamp| stamp.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(state: &mut GrokState, raw: &str) -> Vec<ParsedEvent> {
        parse_at(state, raw, 0)
    }

    fn parse_at(state: &mut GrokState, raw: &str, start: u64) -> Vec<ParsedEvent> {
        let line = TailLine {
            start,
            bytes: raw.as_bytes().to_vec(),
        };
        state.parse_line(Path::new("/home/u/.grok/logs/unified.jsonl"), &line)
    }

    /// The real line verbatim (log line 14828), quoted so `event_id` and the
    /// normalized numbers can be asserted exactly.
    const REAL_LINE: &str = r#"{"ts":"2026-09-10T09:51:07.339Z","src":"shell","pid":1251790,"ver":"1.0.25","lvl":"info","sid":"01a08a84-4c3d-75e2-8a3d-d8eea8c383a5","msg":"shell.turn.inference_done","ctx":{"loop_index":3,"model_elapsed_ms":48865,"elapsed_since_turn_start_ms":118055,"ttft_ms":10999,"itl_p50_ms":0,"attempts":1,"prompt_tokens":323742,"cached_prompt_tokens":321280,"completion_tokens":2792,"reasoning_tokens":506,"tokens_per_sec":73.7}}"#;

    /// The real `model changed` line verbatim (log line 14749).
    const REAL_MODEL_CHANGED: &str = r#"{"ts":"2026-09-10T09:37:34.292Z","src":"shell","pid":1251790,"ver":"1.0.25","lvl":"info","sid":"01a08a84-4c3d-75e2-8a3d-d8eea8c383a5","msg":"model changed","ctx":{"model":"grok-4.6"}}"#;

    fn real_line_with(other_ts: &str) -> String {
        REAL_LINE.replace("2026-09-10T09:51:07.339Z", other_ts)
    }

    #[test]
    fn real_line_normalizes_to_the_measured_numbers() {
        let mut state = GrokState::new(None);
        let events = parse(&mut state, REAL_LINE);
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(
            event.event_id,
            "01a08a84-4c3d-75e2-8a3d-d8eea8c383a5:3:2026-09-10T09:51:07.339Z"
        );
        assert_eq!(event.occurred_at, 1_789_033_867_339);
        assert_eq!(event.usage.input_total, 323742);
        assert_eq!(event.usage.cache_read, 321280);
        assert_eq!(event.usage.cache_write, 0);
        assert_eq!(event.usage.output_total, 2792);
        assert_eq!(event.usage.reasoning, 506);
    }

    #[test]
    fn untracked_sid_without_config_yields_no_model() {
        let mut state = GrokState::new(None);
        let events = parse(&mut state, REAL_LINE);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].model, None);
        assert_eq!(events[0].model_source, None);
    }

    #[test]
    fn untracked_sid_uses_the_injected_default() {
        let mut state = GrokState::new(Some("grok-4.6".to_owned()));
        let events = parse(&mut state, REAL_LINE);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].model.as_deref(), Some("grok-4.6"));
        assert_eq!(events[0].model_source, Some(ModelSource::ConfigFallback));
    }

    #[test]
    fn refreshed_default_applies_to_later_lines_only() {
        let mut state = GrokState::new(Some("grok-4.5".to_owned()));
        let first = parse(&mut state, REAL_LINE);
        assert_eq!(first[0].model.as_deref(), Some("grok-4.5"));

        state.set_default_model(Some("grok-4.7".to_owned()));
        let second = parse(&mut state, &real_line_with("2026-09-10T09:51:07.340Z"));
        assert_eq!(second[0].model.as_deref(), Some("grok-4.7"));
        assert_eq!(second[0].model_source, Some(ModelSource::ConfigFallback));

        state.set_default_model(None);
        let third = parse(&mut state, &real_line_with("2026-09-10T09:51:07.341Z"));
        assert_eq!(third[0].model, None);
        assert_eq!(third[0].model_source, None);
    }

    #[test]
    fn tracked_model_wins_over_the_default() {
        let mut state = GrokState::new(Some("grok-4.5".to_owned()));
        // 1.4s before the usage line, so the usage line is still untracked here.
        let early = parse(&mut state, &real_line_with("2026-09-10T09:51:05.000Z"));
        assert_eq!(early[0].model_source, Some(ModelSource::ConfigFallback));

        // The observed switch, on the same sid.
        let switched = parse_at(&mut state, REAL_MODEL_CHANGED, 1_000);
        assert!(switched.is_empty(), "state updates yield no events");

        let events = parse(&mut state, REAL_LINE);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].model.as_deref(), Some("grok-4.6"));
        assert_eq!(events[0].model_source, Some(ModelSource::Event));
    }

    #[test]
    fn later_model_changed_replaces_the_tracked_model() {
        let mut state = GrokState::new(None);
        parse(&mut state, REAL_MODEL_CHANGED);
        let switch = REAL_MODEL_CHANGED.replace("grok-4.6", "grok-4.5");
        parse_at(&mut state, &switch, 2_000);
        let events = parse(&mut state, REAL_LINE);
        assert_eq!(events[0].model.as_deref(), Some("grok-4.5"));
        assert_eq!(events[0].model_source, Some(ModelSource::Event));
    }

    #[test]
    fn model_changed_for_another_sid_does_not_leak() {
        let mut state = GrokState::new(None);
        let other = REAL_MODEL_CHANGED.replace(
            "01a08a84-4c3d-75e2-8a3d-d8eea8c383a5",
            "01a089f7-71ec-76c1-b2a1-18a1526a0ca9",
        );
        parse(&mut state, &other);
        let events = parse(&mut state, REAL_LINE);
        assert_eq!(events[0].model, None);
        assert_eq!(events[0].model_source, None);
    }

    /// Regression for the measured 8% loss: `sid + loop_index` alone collapses
    /// 1190 events into 1092 keys, so distinct `ts` values MUST produce distinct
    /// ids.
    #[test]
    fn same_sid_and_loop_index_with_different_ts_are_two_events() {
        let mut state = GrokState::new(None);
        let first = parse(&mut state, REAL_LINE);
        let second = parse(&mut state, &real_line_with("2026-09-10T09:51:08.339Z"));
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_ne!(first[0].event_id, second[0].event_id);
        assert_eq!(first[0].occurred_at, 1_789_033_867_339);
        assert_eq!(second[0].occurred_at, 1_789_033_868_339);
    }

    #[test]
    fn forget_drops_tracked_models_but_keeps_the_default() {
        let mut state = GrokState::new(Some("grok-4.6".to_owned()));
        parse(&mut state, REAL_MODEL_CHANGED);
        state.forget(Path::new("/home/u/.grok/logs/unified.jsonl"));
        let events = parse(&mut state, REAL_LINE);
        assert_eq!(events[0].model.as_deref(), Some("grok-4.6"));
        assert_eq!(events[0].model_source, Some(ModelSource::ConfigFallback));
    }

    #[test]
    fn default_state_has_no_model_source() {
        let mut state = GrokState::default();
        let events = parse(&mut state, REAL_LINE);
        assert_eq!(events[0].model, None);
        assert_eq!(events[0].model_source, None);
    }

    #[test]
    fn a_line_without_sid_or_without_ts_is_skipped() {
        let mut state = GrokState::new(Some("grok-4.6".to_owned()));
        let no_sid = REAL_LINE.replace(
            r#""sid":"01a08a84-4c3d-75e2-8a3d-d8eea8c383a5","msg":"shell.turn.inference_done","#,
            r#""msg":"shell.turn.inference_done","#,
        );
        assert!(parse(&mut state, &no_sid).is_empty());

        let no_ts = REAL_LINE.replace(r#""ts":"2026-09-10T09:51:07.339Z","#, "");
        assert!(parse(&mut state, &no_ts).is_empty());

        let bad_ts = REAL_LINE.replace("2026-09-10T09:51:07.339Z", "yesterday");
        assert!(parse(&mut state, &bad_ts).is_empty());
    }

    #[test]
    fn non_json_and_unrelated_messages_yield_nothing() {
        let mut state = GrokState::new(Some("grok-4.6".to_owned()));
        assert!(parse(&mut state, "not json at all").is_empty());
        assert!(parse(&mut state, "").is_empty());
        // Real line 1: no `ctx`, unknown `msg`.
        let phase = r#"{"ts":"2026-09-02T00:16:52.608Z","src":"grok-pager","pid":14985,"ver":"1.0.13","lvl":"debug","sid":"01a05f66-381d-7b00-a620-75add8c8e3b3","msg":"turn.phase_transition","ctx":{"from":"tool_running","to":"waiting_model","phase_elapsed_ms":87}}"#;
        assert!(parse(&mut state, phase).is_empty());
        // Real line 4: usage-shaped `ctx` but a model-related `msg` that is not
        // one of the two we own.
        let backend_switch = r#"{"ts":"2026-09-10T06:18:17.786Z","src":"shell","pid":876120,"ver":"1.0.25","lvl":"info","sid":"01a089f7-71ec-76c1-b2a1-18a1526a0ca9","msg":"backend_search: model switch","ctx":{"new_model":"grok-4.6","api_backend":"Responses","supports_backend_search":true}}"#;
        assert!(parse(&mut state, backend_switch).is_empty());
        // An unrelated message must not disturb the tracked/default model.
        let events = parse(&mut state, REAL_LINE);
        assert_eq!(events[0].model.as_deref(), Some("grok-4.6"));
        assert_eq!(events[0].model_source, Some(ModelSource::ConfigFallback));
    }

    #[test]
    fn malformed_and_missing_ctx_fields_default_to_zero() {
        let mut state = GrokState::new(None);
        // Missing `ctx` entirely, absent numeric fields, and a wrong-typed field.
        let bare =
            r#"{"ts":"2026-09-10T09:51:07.339Z","sid":"s1","msg":"shell.turn.inference_done"}"#;
        let events = parse(&mut state, bare);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].usage,
            UsageEvent {
                input_total: 0,
                cache_read: 0,
                cache_write: 0,
                output_total: 0,
                reasoning: 0,
            }
        );
        assert_eq!(events[0].event_id, "s1:0:2026-09-10T09:51:07.339Z");

        let typed = concat!(
            r#"{"ts":"2026-09-10T09:51:07.339Z","sid":"s1","msg":"shell.turn.inference_done","#,
            r#""ctx":{"loop_index":"3","prompt_tokens":10,"cached_prompt_tokens":null,"#,
            r#""completion_tokens":4,"reasoning_tokens":2}}"#
        );
        let events = parse(&mut state, typed);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].usage.input_total, 10);
        assert_eq!(events[0].usage.cache_read, 0);
        assert_eq!(events[0].usage.output_total, 4);
        assert_eq!(events[0].usage.reasoning, 2);
        // `loop_index` was a string, so it degrades to 0 in the key.
        assert_eq!(events[0].event_id, "s1:0:2026-09-10T09:51:07.339Z");
    }

    #[test]
    fn model_changed_without_a_model_keeps_previous_state() {
        let mut state = GrokState::new(None);
        parse(&mut state, REAL_MODEL_CHANGED);
        let no_model = r#"{"ts":"2026-09-10T09:40:00.000Z","sid":"01a08a84-4c3d-75e2-8a3d-d8eea8c383a5","msg":"model changed","ctx":{}}"#;
        assert!(parse_at(&mut state, no_model, 5_000).is_empty());
        let events = parse(&mut state, REAL_LINE);
        assert_eq!(events[0].model.as_deref(), Some("grok-4.6"));
        assert_eq!(events[0].model_source, Some(ModelSource::Event));
    }
}
