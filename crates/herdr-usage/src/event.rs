//! Canonical usage event and the four source normalizers.
//!
//! Single owner for the unit-of-measure conversion described in plan §4.
//! Downstream code (db, cost, UI) must never re-derive these quantities.

use serde::{Deserialize, Serialize};

/// One billable inference, already normalized to gross input and
/// reasoning-inclusive output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsageEvent {
    /// Gross input: cache hits included (`input + cacheRead` on net-reporting sources).
    pub input_total: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    /// Gross output. `reasoning` is a *component* of this value, never an addend.
    pub output_total: i64,
    pub reasoning: i64,
}

impl UsageEvent {
    /// Clamp to the invariants asserted at insert time (plan §4.3).
    ///
    /// Clamping rather than rejecting keeps a malformed upstream record from
    /// stalling the collector; the clamp is visible in the stored numbers and
    /// the caller logs it.
    pub fn clamped(mut self) -> (Self, bool) {
        let mut adjusted = false;
        let set = |value: &mut i64, low: i64, high: i64, adjusted: &mut bool| {
            let bounded = (*value).clamp(low, high);
            if bounded != *value {
                *value = bounded;
                *adjusted = true;
            }
        };
        for field in [
            &mut self.input_total,
            &mut self.cache_read,
            &mut self.cache_write,
            &mut self.output_total,
            &mut self.reasoning,
        ] {
            set(field, 0, i64::MAX, &mut adjusted);
        }
        if self.cache_read > self.input_total {
            self.cache_read = self.input_total;
            adjusted = true;
        }
        if self.reasoning > self.output_total {
            self.reasoning = self.output_total;
            adjusted = true;
        }
        (self, adjusted)
    }

    /// Ranking / summary quantity (plan §9.4): gross input plus gross output.
    pub fn total_tokens(&self) -> i64 {
        self.input_total.saturating_add(self.output_total)
    }
}

/// How `usage_event.model` was obtained (plan §8.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelSource {
    /// Reported on the event itself (omp, opencode).
    Event,
    /// Recovered from an earlier `turn_context` in the same file (codex).
    Context,
    /// Fell back to `config.toml [models].default` (grok).
    ConfigFallback,
}

impl ModelSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ModelSource::Event => "event",
            ModelSource::Context => "context",
            ModelSource::ConfigFallback => "config_fallback",
        }
    }
}

/// A parsed event ready to be written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedEvent {
    pub event_id: String,
    pub usage: UsageEvent,
    pub model: Option<String>,
    pub model_source: Option<ModelSource>,
    /// Upstream account/provider label. omp reports `message.provider`
    /// (e.g. `openai-codex`, `google-antigravity`, `opencode-go`, `codebuddy`);
    /// opencode reports `providerID`; codex and grok logs carry none.
    pub provider: Option<String>,
    /// Session identity only. Conversation text is deliberately never stored.
    pub session_id: Option<String>,
    /// End-to-end request timing, in UTC epoch milliseconds.
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
    /// Stable credential identity and a safe display label. These live on each
    /// request (not on the session) because a session may cross accounts.
    pub account_key: Option<String>,
    pub account_label: Option<String>,
    pub account_source: Option<String>,
    /// UTC epoch milliseconds.
    pub occurred_at: i64,
}

// ---------------------------------------------------------------------------
// omp
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct OmpUsage {
    pub input: i64,
    pub output: i64,
    #[serde(rename = "cacheRead")]
    pub cache_read: i64,
    #[serde(rename = "cacheWrite")]
    pub cache_write: i64,
    #[serde(rename = "reasoningTokens")]
    pub reasoning_tokens: Option<i64>,
}

/// omp reports `input` net of cache (measured identity:
/// `total = input + output + cacheRead + cacheWrite`).
pub fn normalize_omp(usage: &OmpUsage) -> UsageEvent {
    UsageEvent {
        input_total: usage.input.saturating_add(usage.cache_read),
        cache_read: usage.cache_read,
        cache_write: usage.cache_write,
        // reasoning is already inside `output`.
        output_total: usage.output,
        reasoning: usage.reasoning_tokens.unwrap_or(0),
    }
}

// ---------------------------------------------------------------------------
// codex
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CodexLastUsage {
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub cache_write_input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
}

/// codex reports `input_tokens` already gross (measured: `total = input + output`,
/// `cached <= input`).
pub fn normalize_codex(usage: &CodexLastUsage) -> UsageEvent {
    UsageEvent {
        input_total: usage.input_tokens,
        cache_read: usage.cached_input_tokens,
        cache_write: usage.cache_write_input_tokens,
        output_total: usage.output_tokens,
        reasoning: usage.reasoning_output_tokens,
    }
}

// ---------------------------------------------------------------------------
// grok
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct GrokCtx {
    pub prompt_tokens: i64,
    pub cached_prompt_tokens: i64,
    pub completion_tokens: i64,
    pub reasoning_tokens: i64,
}

/// grok reports `prompt_tokens` gross and has no cache-write field.
/// `completion_tokens` is the total output side (proved by
/// `tokens_per_sec == completion / (elapsed - ttft)`).
pub fn normalize_grok(ctx: &GrokCtx) -> UsageEvent {
    UsageEvent {
        input_total: ctx.prompt_tokens,
        cache_read: ctx.cached_prompt_tokens,
        cache_write: 0,
        output_total: ctx.completion_tokens,
        reasoning: ctx.reasoning_tokens,
    }
}

// ---------------------------------------------------------------------------
// opencode
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct OcCache {
    pub read: i64,
    pub write: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct OcTokens {
    pub input: i64,
    pub output: i64,
    pub reasoning: i64,
    pub cache: OcCache,
}

/// opencode reports `input` net of cache and `reasoning` *outside* `output`
/// (measured: 27888 rows with `reasoning > output`).
pub fn normalize_opencode(tokens: &OcTokens) -> UsageEvent {
    UsageEvent {
        input_total: tokens.input.saturating_add(tokens.cache.read),
        cache_read: tokens.cache.read,
        cache_write: tokens.cache.write,
        output_total: tokens.output.saturating_add(tokens.reasoning),
        reasoning: tokens.reasoning,
    }
}

/// Wire form persisted to `collect_offset.cursor`.
///
/// The trailing partial line is persisted too: without it, a restart that lands
/// mid-line would re-read the fragment as if it were a whole line, parse it as
/// invalid JSON, discard it, and lose the completed record for good.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FileOffsets {
    /// path -> offset just past the last consumed byte.
    pub offsets: std::collections::BTreeMap<String, u64>,
    /// path -> absolute offset where `residues[path]` began, plus its bytes.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub residues: std::collections::BTreeMap<String, Residue>,
}

/// A trailing fragment that did not yet end in a newline.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Residue {
    pub start: u64,
    /// Base64-free encoding: the fragment is JSON text, so it is stored as a
    /// string. A malformed (non-UTF-8) fragment is dropped at read time.
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omp_adds_cache_read_back_into_input() {
        let event = normalize_omp(&OmpUsage {
            input: 14438,
            output: 100,
            cache_read: 75008,
            cache_write: 0,
            reasoning_tokens: Some(40),
        });
        assert_eq!(event.input_total, 89446);
        assert_eq!(event.output_total, 100);
        assert_eq!(event.reasoning, 40);
    }

    #[test]
    fn codex_keeps_gross_input_untouched() {
        let event = normalize_codex(&CodexLastUsage {
            input_tokens: 12960,
            cached_input_tokens: 4480,
            output_tokens: 327,
            reasoning_output_tokens: 69,
            ..CodexLastUsage::default()
        });
        assert_eq!(event.input_total, 12960);
        assert_eq!(event.cache_read, 4480);
        assert_eq!(event.output_total, 327);
        assert_eq!(event.reasoning, 69);
    }

    #[test]
    fn grok_has_no_cache_write_and_keeps_prompt_gross() {
        let event = normalize_grok(&GrokCtx {
            prompt_tokens: 323742,
            cached_prompt_tokens: 321280,
            completion_tokens: 2792,
            reasoning_tokens: 506,
        });
        assert_eq!(event.input_total, 323742);
        assert_eq!(event.cache_write, 0);
        assert_eq!(event.output_total, 2792);
    }

    #[test]
    fn opencode_folds_reasoning_into_output() {
        // Measured row 147799: reasoning exceeds the non-reasoning output.
        let event = normalize_opencode(&OcTokens {
            input: 14438,
            output: 100,
            reasoning: 900,
            cache: OcCache {
                read: 75008,
                write: 12,
            },
        });
        assert_eq!(event.input_total, 89446);
        assert_eq!(event.output_total, 1000);
        assert_eq!(event.reasoning, 900);
    }

    #[test]
    fn clamping_repairs_violations_without_dropping_the_event() {
        let (event, adjusted) = UsageEvent {
            input_total: 10,
            cache_read: 50,
            cache_write: 0,
            output_total: 5,
            reasoning: 9,
        }
        .clamped();
        assert!(adjusted);
        assert_eq!(event.cache_read, 10);
        assert_eq!(event.reasoning, 5);

        let (_, adjusted) = normalize_grok(&GrokCtx::default()).clamped();
        assert!(!adjusted);
    }
}
