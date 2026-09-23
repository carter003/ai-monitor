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
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
struct PendingMessage {
    event_id: String,
    usage: crate::event::UsageEvent,
    model: Option<String>,
    provider: Option<String>,
    session_id: Option<String>,
    started_at: Option<i64>,
    completed_at: Option<i64>,
    duration_ms: Option<i64>,
    occurred_at: i64,
}

#[derive(Default)]
pub struct OmpState {
    base_sessions: HashMap<PathBuf, String>,
    sessions: HashMap<PathBuf, String>,
    pins: HashMap<(PathBuf, String), String>,
    api_key_pins: HashMap<(PathBuf, String), String>,
    api_key_timelines: HashSet<(PathBuf, String)>,
    seeded: HashSet<PathBuf>,
    just_reset: HashSet<(PathBuf, String)>,
    pending: HashMap<PathBuf, PendingMessage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountIdentity {
    pub key: String,
    pub label: String,
    pub source: &'static str,
}

/// Safe account identities reconstructed from OMP's auth database. Secrets are
/// read only long enough to hash them and are never returned or persisted.
#[derive(Default)]
pub struct AccountCatalog {
    by_pin: HashMap<(String, String), AccountIdentity>,
    by_id: HashMap<(String, String), AccountIdentity>,
    sticky: HashMap<(String, String), AccountIdentity>,
    single: HashMap<String, AccountIdentity>,
}

const API_KEY_STICKY_ENTRY: &str = "herdr-api-key-sticky-v1";

impl AccountCatalog {
    pub fn load(path: &Path) -> Self {
        let Ok(connection) = rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) else {
            return Self::default();
        };
        let mut catalog = Self::default();
        let mut by_provider = HashMap::<String, Vec<AccountIdentity>>::new();
        let Ok(mut statement) = connection.prepare(
            "SELECT id, provider, credential_type, data FROM auth_credentials
             WHERE provider IN ('google-antigravity', 'opencode-go') ORDER BY id",
        ) else {
            return catalog;
        };
        let Ok(rows) = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        }) else {
            return catalog;
        };
        for row in rows.filter_map(Result::ok) {
            let (id, provider, kind, data) = row;
            let Ok(data) = serde_json::from_str::<Value>(&data) else {
                continue;
            };
            let identity = if kind == "oauth" {
                let field = |name| data.get(name).and_then(Value::as_str).unwrap_or("");
                let email = field("email");
                let raw = [
                    provider.as_str(),
                    field("accountId"),
                    email,
                    field("orgId"),
                    field("projectId"),
                ]
                .join("\0");
                let pin = sha256(&raw);
                let identity = AccountIdentity {
                    key: format!("oauth:{pin}"),
                    label: if email.is_empty() {
                        format!("Antigravity 账户 {}", &pin[..8])
                    } else {
                        email.to_owned()
                    },
                    source: "credential_pin",
                };
                catalog
                    .by_pin
                    .insert((provider.clone(), pin), identity.clone());
                identity
            } else if kind == "api_key" {
                let Some(secret) = data.get("key").and_then(Value::as_str) else {
                    continue;
                };
                let fingerprint = sha256(secret);
                AccountIdentity {
                    key: format!("api_key:{fingerprint}"),
                    label: format!("OpenCode Go Key {}", &fingerprint[..8]),
                    source: "sticky_cache",
                }
            } else {
                continue;
            };
            by_provider
                .entry(provider.clone())
                .or_default()
                .push(identity.clone());
            catalog.by_id.insert((provider, id.to_string()), identity);
        }
        for (provider, identities) in by_provider {
            if identities.len() == 1 {
                let mut identity = identities[0].clone();
                identity.source = "single_credential";
                catalog.single.insert(provider, identity);
            }
        }
        drop(statement);
        let Ok(mut statement) =
            connection.prepare("SELECT key, value FROM cache WHERE key LIKE 'session:sticky:%'")
        else {
            return catalog;
        };
        let Ok(rows) = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) else {
            return catalog;
        };
        for (key, value) in rows.filter_map(Result::ok) {
            let mut parts = key.splitn(4, ':');
            if parts.next() != Some("session") || parts.next() != Some("sticky") {
                continue;
            }
            let (Some(provider), Some(session)) = (parts.next(), parts.next()) else {
                continue;
            };
            let Some(id) = serde_json::from_str::<Value>(&value)
                .ok()
                .and_then(|value| value.get("credentialId").and_then(Value::as_i64))
            else {
                continue;
            };
            let Some(identity) = catalog.by_id.get(&(provider.to_owned(), id.to_string())) else {
                continue;
            };
            let mut identity = identity.clone();
            identity.source = "sticky_cache";
            catalog
                .sticky
                .insert((provider.to_owned(), session.to_owned()), identity);
        }
        catalog
    }

    fn resolve(
        &self,
        provider: &str,
        session: Option<&str>,
        pin: Option<&str>,
    ) -> Option<AccountIdentity> {
        if let Some(pin) = pin {
            if let Some(identity) = self.by_pin.get(&(provider.to_owned(), pin.to_owned())) {
                return Some(identity.clone());
            }
        }
        if let Some(session) = session {
            if let Some(identity) = self.sticky.get(&(provider.to_owned(), session.to_owned())) {
                return Some(identity.clone());
            }
        }
        self.single.get(provider).cloned()
    }

    fn resolve_credential_id(
        &self,
        provider: &str,
        credential_id: &str,
    ) -> Option<AccountIdentity> {
        self.by_id
            .get(&(provider.to_owned(), credential_id.to_owned()))
            .cloned()
            .map(|mut identity| {
                identity.source = "session_pin";
                identity
            })
    }
}

fn sha256(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

impl OmpState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop metadata tied to a rotated session file.
    /// Drop metadata tied to a rotated session file.
    pub fn forget(&mut self, path: &Path) {
        self.base_sessions.remove(path);
        self.sessions.remove(path);
        self.pins.retain(|(file, _), _| file != path);
        self.api_key_pins.retain(|(file, _), _| file != path);
        self.api_key_timelines.retain(|(file, _)| file != path);
        self.seeded.remove(path);
        self.just_reset.retain(|(file, _)| file != path);
        self.pending.remove(path);
    }

    fn finalize_pending(
        &mut self,
        path: &Path,
        accounts: &AccountCatalog,
    ) -> Option<ParsedEvent> {
        let pending = self.pending.remove(path)?;
        let pin = pending
            .provider
            .as_ref()
            .and_then(|provider| self.pins.get(&(path.to_owned(), provider.clone())));
        let account = pending.provider.as_deref().and_then(|provider| {
            let timeline_key = (path.to_owned(), provider.to_owned());
            if self.api_key_timelines.contains(&timeline_key) {
                self.api_key_pins
                    .get(&timeline_key)
                    .and_then(|id| accounts.resolve_credential_id(provider, id))
            } else {
                accounts.resolve(
                    provider,
                    pending.session_id.as_deref(),
                    pin.map(String::as_str),
                )
            }
        });
        Some(ParsedEvent {
            event_id: pending.event_id,
            usage: pending.usage,
            model_source: pending.model.as_ref().map(|_| ModelSource::Event),
            model: pending.model,
            provider: pending.provider,
            session_id: pending.session_id,
            started_at: pending.started_at,
            completed_at: pending.completed_at,
            duration_ms: pending.duration_ms,
            account_key: account.as_ref().map(|account| account.key.clone()),
            account_label: account.as_ref().map(|account| account.label.clone()),
            account_source: account.map(|account| account.source.to_owned()),
            occurred_at: pending.occurred_at,
        })
    }

    pub fn flush_file(
        &mut self,
        path: &Path,
        accounts: &AccountCatalog,
    ) -> Vec<ParsedEvent> {
        self.finalize_pending(path, accounts).into_iter().collect()
    }

    /// Read JSONL metadata so a collector restart can associate
    /// newly appended requests with the upstream session id and credentials.
    pub fn seed_file(&mut self, path: &Path) {
        if !self.seeded.insert(path.to_owned()) {
            return;
        }
        let Ok(file) = std::fs::File::open(path) else {
            return;
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let bytes = line.as_bytes();
            if !bytes.windows(9).any(|part| part == b"\"session\"")
                && !bytes.windows(14).any(|part| part == b"credential_pin")
                && !bytes.windows(14).any(|part| part == b"reset_boundary")
                && !bytes
                    .windows(b"herdr-api-key-sticky-v1".len())
                    .any(|part| part == b"herdr-api-key-sticky-v1")
            {
                continue;
            }
            let Ok(record) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match record.get("type").and_then(Value::as_str) {
                Some("session") => {
                    if let Some(id) = record.get("id").and_then(Value::as_str) {
                        self.base_sessions.insert(path.to_owned(), id.to_owned());
                        self.sessions.insert(path.to_owned(), id.to_owned());
                    }
                }
                Some("reset_boundary") => {
                    let reset_id = record.get("id").and_then(Value::as_str);
                    let base_id = self
                        .base_sessions
                        .get(path)
                        .cloned()
                        .or_else(|| self.sessions.get(path).cloned());
                    if let (Some(base), Some(rid)) = (base_id, reset_id) {
                        self.sessions.insert(path.to_owned(), format!("{base}/{rid}"));
                        self.just_reset.insert((path.to_owned(), "all".to_string()));
                    }
                }
                Some("credential_pin") => {
                    if let (Some(provider), Some(hash)) = (
                        record.get("provider").and_then(Value::as_str),
                        record.get("hash").and_then(Value::as_str),
                    ) {
                        self.pins
                            .insert((path.to_owned(), provider.to_owned()), hash.to_owned());
                    }
                }
                Some("custom")
                    if record.get("customType").and_then(Value::as_str)
                        == Some(API_KEY_STICKY_ENTRY) =>
                {
                    let Some(data) = record.get("data") else {
                        continue;
                    };
                    let (Some(provider), Some(action)) = (
                        data.get("provider").and_then(Value::as_str),
                        data.get("action").and_then(Value::as_str),
                    ) else {
                        continue;
                    };
                    let key = (path.to_owned(), provider.to_owned());
                    match action {
                        "pin" => {
                            if let Some(cid) = data.get("credentialId").and_then(credential_id) {
                                self.api_key_timelines.insert(key.clone());
                                self.api_key_pins.insert(key, cid);
                            }
                        }
                        "release" => {
                            self.api_key_timelines.insert(key.clone());
                            self.api_key_pins.remove(&key);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    pub fn parse_line(&mut self, path: &Path, line: &TailLine) -> Vec<ParsedEvent> {
        let mut events = self.parse_line_with_accounts(path, line, &AccountCatalog::default());
        events.extend(self.flush_file(path, &AccountCatalog::default()));
        events
    }

    pub fn parse_line_with_accounts(
        &mut self,
        path: &Path,
        line: &TailLine,
        accounts: &AccountCatalog,
    ) -> Vec<ParsedEvent> {
        if line.bytes.is_empty() {
            return vec![];
        }
        let Ok(record) = serde_json::from_slice::<Value>(&line.bytes) else {
            return vec![];
        };
        match record.get("type").and_then(Value::as_str) {
            Some("session") => {
                let out = self.finalize_pending(path, accounts).into_iter().collect::<Vec<_>>();
                if let Some(id) = record.get("id").and_then(Value::as_str) {
                    self.base_sessions.insert(path.to_owned(), id.to_owned());
                    self.sessions.insert(path.to_owned(), id.to_owned());
                }
                return out;
            }
            Some("reset_boundary") => {
                let out = self.finalize_pending(path, accounts).into_iter().collect::<Vec<_>>();
                let reset_id = record.get("id").and_then(Value::as_str);
                let base_id = self
                    .base_sessions
                    .get(path)
                    .cloned()
                    .or_else(|| self.sessions.get(path).cloned());
                if let (Some(base), Some(rid)) = (base_id, reset_id) {
                    self.sessions.insert(path.to_owned(), format!("{base}/{rid}"));
                    self.just_reset.insert((path.to_owned(), "all".to_string()));
                }
                return out;
            }
            Some("credential_pin") => {
                if let (Some(provider), Some(hash)) = (
                    record.get("provider").and_then(Value::as_str),
                    record.get("hash").and_then(Value::as_str),
                ) {
                    self.pins
                        .insert((path.to_owned(), provider.to_owned()), hash.to_owned());
                }
                let mut out = vec![];
                if let Some(pending) = self.pending.get(path) {
                    if pending.provider.as_deref() == record.get("provider").and_then(Value::as_str) {
                        if let Some(event) = self.finalize_pending(path, accounts) {
                            out.push(event);
                        }
                    }
                }
                return out;
            }
            Some("custom")
                if record.get("customType").and_then(Value::as_str)
                    == Some("tool_execution_start") =>
            {
                // tool_execution_start sits between assistant message and its credential_pin.
                // Keep pending event untouched so credential_pin on the next line can attach to it.
                return vec![];
            }
            Some("custom")
                if record.get("customType").and_then(Value::as_str)
                    == Some(API_KEY_STICKY_ENTRY) =>
            {
                let out = self.finalize_pending(path, accounts).into_iter().collect::<Vec<_>>();
                let Some(data) = record.get("data") else {
                    return out;
                };
                let (Some(provider), Some(session_id), Some(action)) = (
                    data.get("provider").and_then(Value::as_str),
                    data.get("sessionId").and_then(Value::as_str),
                    data.get("action").and_then(Value::as_str),
                ) else {
                    return out;
                };
                let base_matches = self
                    .base_sessions
                    .get(path)
                    .map(String::as_str)
                    == Some(session_id);
                let current_matches = self
                    .sessions
                    .get(path)
                    .map(String::as_str)
                    == Some(session_id);
                if !base_matches && !current_matches {
                    return out;
                }
                let key = (path.to_owned(), provider.to_owned());
                match action {
                    "pin" => {
                        if let Some(credential_id) =
                            data.get("credentialId").and_then(credential_id)
                        {
                            self.api_key_timelines.insert(key.clone());
                            self.api_key_pins.insert(key, credential_id);
                        }
                    }
                    "release" => {
                        self.api_key_timelines.insert(key.clone());
                        self.api_key_pins.remove(&key);
                    }
                    _ => {}
                }
                return out;
            }
            _ => {}
        }
        let Some(message) = record.get("message") else {
            return self.finalize_pending(path, accounts).into_iter().collect();
        };
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            return self.finalize_pending(path, accounts).into_iter().collect();
        }
        let mut out = self.finalize_pending(path, accounts).into_iter().collect::<Vec<_>>();
        let Some(usage) = message.get("usage") else {
            return out;
        };
        if usage.is_null() {
            return out;
        }
        let Some(event_id) = record.get("id").and_then(Value::as_str) else {
            return out;
        };
        let Some(occurred_at) = message
            .get("timestamp")
            .and_then(timestamp_ms)
            .or_else(|| record.get("timestamp").and_then(timestamp_ms))
        else {
            return out;
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
        let provider = message
            .get("provider")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned);
        let usage = normalize_omp(&parsed);
        let session_id = self.sessions.get(path).cloned();
        let duration_ms = message
            .get("duration")
            .and_then(Value::as_f64)
            .map(|value| value.max(0.0).round() as i64);
        let completed_at = message
            .get("completedAt")
            .and_then(timestamp_ms)
            .or_else(|| duration_ms.map(|duration| occurred_at.saturating_add(duration)));

        let has_pin = provider
            .as_ref()
            .is_some_and(|p| self.pins.contains_key(&(path.to_owned(), p.clone())));
        let is_reset = self.just_reset.remove(&(path.to_owned(), "all".to_string()))
            || provider
                .as_ref()
                .is_some_and(|p| self.just_reset.remove(&(path.to_owned(), p.clone())));
        let needs_pin_wait =
            provider.as_deref() == Some("google-antigravity") && (!has_pin || is_reset);

        if !needs_pin_wait {
            let pin = provider
                .as_ref()
                .and_then(|p| self.pins.get(&(path.to_owned(), p.clone())));
            let account = provider.as_deref().and_then(|provider| {
                let timeline_key = (path.to_owned(), provider.to_owned());
                if self.api_key_timelines.contains(&timeline_key) {
                    self.api_key_pins
                        .get(&timeline_key)
                        .and_then(|id| accounts.resolve_credential_id(provider, id))
                } else {
                    accounts.resolve(provider, session_id.as_deref(), pin.map(String::as_str))
                }
            });
            out.push(ParsedEvent {
                event_id: event_id.to_owned(),
                usage,
                model_source: model.as_ref().map(|_| ModelSource::Event),
                model,
                provider,
                session_id,
                started_at: Some(occurred_at),
                completed_at,
                duration_ms,
                account_key: account.as_ref().map(|account| account.key.clone()),
                account_label: account.as_ref().map(|account| account.label.clone()),
                account_source: account.map(|account| account.source.to_owned()),
                occurred_at,
            });
        } else {
            self.pending.insert(
                path.to_owned(),
                PendingMessage {
                    event_id: event_id.to_owned(),
                    usage,
                    model,
                    provider,
                    session_id,
                    started_at: Some(occurred_at),
                    completed_at,
                    duration_ms,
                    occurred_at,
                },
            );
        }
        out
    }
}

/// Read an integer field, treating a missing or non-numeric value as 0.
fn int(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// `(envelope id, message.provider)` of an assistant record.
///
/// Used only by the one-off `provider` backfill, which must re-read session
/// lines written before the column existed. `None` when the line is not JSON or
/// carries no non-empty provider.
pub fn record_provider(line: &[u8]) -> Option<(String, String)> {
    let record: Value = serde_json::from_slice(line).ok()?;
    let id = record.get("id").and_then(Value::as_str)?;
    let provider = record
        .pointer("/message/provider")
        .and_then(Value::as_str)
        .filter(|provider| !provider.is_empty())?;
    Some((id.to_owned(), provider.to_owned()))
}

/// Parse an RFC3339 timestamp into UTC epoch milliseconds.
pub fn parse_rfc3339_ms(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|at| at.timestamp_millis())
}

fn timestamp_ms(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(parse_rfc3339_ms))
}

fn credential_id(value: &Value) -> Option<String> {
    value.as_i64().map(|id| id.to_string()).or_else(|| {
        value
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
    })
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
    fn the_message_provider_is_carried_through() {
        let mut state = OmpState::new();
        let events = state.parse_line(Path::new("/a.jsonl"), &line(0, ASSISTANT));
        assert_eq!(events[0].provider.as_deref(), Some("codebuddy"));
        // An assistant record without a provider yields None rather than an
        // empty string, so the column stays NULL for it.
        let no_provider = ASSISTANT.replace("\"provider\":\"codebuddy\",", "");
        let events = state.parse_line(Path::new("/a.jsonl"), &line(0, &no_provider));
        assert_eq!(events[0].provider, None);
        let blank = ASSISTANT.replace("\"provider\":\"codebuddy\"", "\"provider\":\"\"");
        let events = state.parse_line(Path::new("/a.jsonl"), &line(0, &blank));
        assert_eq!(events[0].provider, None);
    }

    #[test]
    fn record_provider_reads_the_pair_the_backfill_needs() {
        assert_eq!(
            record_provider(ASSISTANT.as_bytes()),
            Some(("681cad2e".to_owned(), "codebuddy".to_owned()))
        );
        // Non-assistant records and malformed lines yield nothing instead of
        // writing a partial mapping into the backfill.
        assert_eq!(record_provider(b"not json"), None);
        assert_eq!(record_provider(br#"{"type":"custom","id":"c1"}"#), None);
        assert_eq!(
            record_provider(br#"{"id":"x","message":{"role":"assistant"}}"#),
            None
        );
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

    #[test]
    fn request_metadata_uses_the_pin_on_each_request_not_a_session_owner() {
        let path = Path::new("/session.jsonl");
        let mut state = OmpState::new();
        let mut accounts = AccountCatalog::default();
        for (hash, label) in [
            ("pin-a", "agy-a@example.com"),
            ("pin-b", "agy-b@example.com"),
        ] {
            accounts.by_pin.insert(
                ("google-antigravity".into(), hash.into()),
                AccountIdentity {
                    key: format!("oauth:{hash}"),
                    label: label.into(),
                    source: "credential_pin",
                },
            );
        }
        let session = r#"{"type":"session","id":"session-1"}"#;
        let pin_a = r#"{"type":"credential_pin","provider":"google-antigravity","hash":"pin-a"}"#;
        let pin_b = r#"{"type":"credential_pin","provider":"google-antigravity","hash":"pin-b"}"#;
        let request = |id: &str, at: i64| {
            format!(
                r#"{{"id":"{id}","type":"message","message":{{"role":"assistant","provider":"google-antigravity","model":"gemini","timestamp":{at},"duration":1250.4,"usage":{{"input":1,"output":2}}}}}}"#
            )
        };
        assert!(state
            .parse_line_with_accounts(path, &line(0, session), &accounts)
            .is_empty());
        assert!(state
            .parse_line_with_accounts(path, &line(1, pin_a), &accounts)
            .is_empty());
        let first = state
            .parse_line_with_accounts(path, &line(2, &request("a", 1000)), &accounts)
            .remove(0);
        assert!(state
            .parse_line_with_accounts(path, &line(3, pin_b), &accounts)
            .is_empty());
        let second = state
            .parse_line_with_accounts(path, &line(4, &request("b", 3000)), &accounts)
            .remove(0);

        assert_eq!(first.session_id.as_deref(), Some("session-1"));
        assert_eq!(first.account_label.as_deref(), Some("agy-a@example.com"));
        assert_eq!(second.account_label.as_deref(), Some("agy-b@example.com"));
        assert_eq!(second.started_at, Some(3000));
        assert_eq!(second.completed_at, Some(4250));
        assert_eq!(second.duration_ms, Some(1250));
    }

    #[test]
    fn opencode_requests_follow_the_persisted_api_key_timeline() {
        let path = Path::new("/session.jsonl");
        let mut state = OmpState::new();
        let mut accounts = AccountCatalog::default();
        for (id, fingerprint) in [("7", "aaaa1111"), ("8", "bbbb2222")] {
            accounts.by_id.insert(
                ("opencode-go".into(), id.into()),
                AccountIdentity {
                    key: format!("api_key:{fingerprint}"),
                    label: format!("OpenCode Go Key {fingerprint}"),
                    source: "sticky_cache",
                },
            );
        }
        let parse = |state: &mut OmpState, ordinal, text: &str| {
            state.parse_line_with_accounts(path, &line(ordinal, text), &accounts)
        };
        let session = r#"{"type":"session","id":"session-1"}"#;
        let pin_a = r#"{"type":"custom","customType":"herdr-api-key-sticky-v1","data":{"action":"pin","provider":"opencode-go","sessionId":"session-1","credentialId":7,"at":100}}"#;
        let release = r#"{"type":"custom","customType":"herdr-api-key-sticky-v1","data":{"action":"release","provider":"opencode-go","sessionId":"session-1","reason":"rotation","at":200}}"#;
        let parent_pin = r#"{"type":"custom","customType":"herdr-api-key-sticky-v1","data":{"action":"pin","provider":"opencode-go","sessionId":"parent-session","credentialId":7,"at":250}}"#;
        let pin_b = r#"{"type":"custom","customType":"herdr-api-key-sticky-v1","data":{"action":"pin","provider":"opencode-go","sessionId":"session-1","credentialId":"8","at":300}}"#;
        let request = |id: &str, at: i64| {
            format!(
                r#"{{"id":"{id}","type":"message","message":{{"role":"assistant","provider":"opencode-go","model":"m","timestamp":{at},"duration":10,"usage":{{"input":1,"output":2}}}}}}"#
            )
        };

        assert!(parse(&mut state, 0, session).is_empty());
        assert!(parse(&mut state, 1, pin_a).is_empty());
        let first = parse(&mut state, 2, &request("a", 1000)).remove(0);
        assert!(parse(&mut state, 3, release).is_empty());
        assert!(parse(&mut state, 4, parent_pin).is_empty());
        assert!(parse(&mut state, 5, pin_b).is_empty());
        let second = parse(&mut state, 6, &request("b", 2000)).remove(0);

        assert_eq!(
            first.account_label.as_deref(),
            Some("OpenCode Go Key aaaa1111")
        );
        assert_eq!(
            second.account_label.as_deref(),
            Some("OpenCode Go Key bbbb2222")
        );
        assert_eq!(first.account_source.as_deref(), Some("session_pin"));
        assert_eq!(second.account_source.as_deref(), Some("session_pin"));
    }

    #[test]
    fn credential_pin_trailing_assistant_message_resolves_correctly() {
        let path = Path::new("/session.jsonl");
        let mut state = OmpState::new();
        let mut accounts = AccountCatalog::default();
        accounts.by_pin.insert(
            ("google-antigravity".into(), "pin-real".into()),
            AccountIdentity {
                key: "oauth:pin-real".into(),
                label: "real@example.com".into(),
                source: "credential_pin",
            },
        );
        // Suppose sticky_cache has an old or pre-pinned account
        accounts.sticky.insert(
            ("google-antigravity".into(), "session-1".into()),
            AccountIdentity {
                key: "oauth:pin-fake".into(),
                label: "fake@example.com".into(),
                source: "sticky_cache",
            },
        );

        let parse = |state: &mut OmpState, ordinal, text: &str| {
            state.parse_line_with_accounts(path, &line(ordinal, text), &accounts)
        };

        let session = r#"{"type":"session","id":"session-1"}"#;
        let request = r#"{"id":"req-1","type":"message","message":{"role":"assistant","provider":"google-antigravity","model":"gemini-3.8-flash","timestamp":1000,"duration":500,"usage":{"input":10,"output":20}}}"#;
        let tool_start = r#"{"type":"custom","customType":"tool_execution_start","data":{"toolName":"bash"}}"#;
        let pin = r#"{"type":"credential_pin","provider":"google-antigravity","hash":"pin-real"}"#;

        assert!(parse(&mut state, 0, session).is_empty());
        // Message arrives first; it should wait for credential_pin rather than immediately resolving to sticky_cache
        assert!(parse(&mut state, 1, request).is_empty());
        // tool_execution_start sits between message and pin
        assert!(parse(&mut state, 2, tool_start).is_empty());
        // When credential_pin arrives, the buffered message is resolved with the new pin
        let events = parse(&mut state, 3, pin);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_id, "req-1");
        assert_eq!(events[0].account_label.as_deref(), Some("real@example.com"));
        assert_eq!(events[0].account_source.as_deref(), Some("credential_pin"));
        assert_eq!(events[0].session_id.as_deref(), Some("session-1"));
    }

    #[test]
    fn reset_boundary_segments_session_and_supports_reranking() {
        let path = Path::new("/session.jsonl");
        let mut state = OmpState::new();
        let mut accounts = AccountCatalog::default();
        accounts.by_pin.insert(
            ("google-antigravity".into(), "pin-a".into()),
            AccountIdentity {
                key: "oauth:pin-a".into(),
                label: "account-a@example.com".into(),
                source: "credential_pin",
            },
        );
        accounts.by_pin.insert(
            ("google-antigravity".into(), "pin-b".into()),
            AccountIdentity {
                key: "oauth:pin-b".into(),
                label: "account-b@example.com".into(),
                source: "credential_pin",
            },
        );

        let parse = |state: &mut OmpState, ordinal, text: &str| {
            state.parse_line_with_accounts(path, &line(ordinal, text), &accounts)
        };

        let session = r#"{"type":"session","id":"session-root"}"#;
        let req_1 = r#"{"id":"req-1","type":"message","message":{"role":"assistant","provider":"google-antigravity","model":"gemini","timestamp":1000,"duration":100,"usage":{"input":1,"output":2}}}"#;
        let pin_a = r#"{"type":"credential_pin","provider":"google-antigravity","hash":"pin-a"}"#;

        let reset = r#"{"type":"reset_boundary","id":"reset-100"}"#;
        let req_2 = r#"{"id":"req-2","type":"message","message":{"role":"assistant","provider":"google-antigravity","model":"gemini","timestamp":2000,"duration":100,"usage":{"input":3,"output":4}}}"#;
        let pin_b = r#"{"type":"credential_pin","provider":"google-antigravity","hash":"pin-b"}"#;

        assert!(parse(&mut state, 0, session).is_empty());
        assert!(parse(&mut state, 1, req_1).is_empty());
        let first = parse(&mut state, 2, pin_a);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].session_id.as_deref(), Some("session-root"));
        assert_eq!(first[0].account_label.as_deref(), Some("account-a@example.com"));

        // Reset boundary creates a new segment
        assert!(parse(&mut state, 3, reset).is_empty());
        // First request after reset waits for possible new pin
        assert!(parse(&mut state, 4, req_2).is_empty());
        // New pin arrives
        let second = parse(&mut state, 5, pin_b);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].session_id.as_deref(), Some("session-root/reset-100"));
        assert_eq!(second[0].account_label.as_deref(), Some("account-b@example.com"));
    }
}
