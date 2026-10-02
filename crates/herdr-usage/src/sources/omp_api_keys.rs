//! Session-keyed API-key evidence, including pins written into a sibling log.

use super::omp::{credential_id, timestamp_ms, AccountCatalog, AccountIdentity};
use serde::Deserialize;
use serde_json::Value;
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

const ENTRY: &str = "herdr-api-key-sticky-v1";

#[derive(Default)]
pub(super) struct ApiKeyHistory {
    scopes: HashMap<PathBuf, HashMap<String, Vec<Change>>>,
    dirty: HashSet<String>,
}

#[derive(PartialEq, Eq)]
struct Change {
    at: i64,
    record_id: Option<String>,
    credential: Option<String>,
    reason: Option<String>,
}

// Unknown message/content fields are skipped by serde without allocating them.
#[derive(Deserialize)]
pub(super) struct Metadata<'a> {
    #[serde(rename = "type", borrow)]
    pub kind: Cow<'a, str>,
    #[serde(borrow)]
    pub id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub provider: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub hash: Option<Cow<'a, str>>,
    #[serde(rename = "customType", borrow)]
    pub custom_type: Option<Cow<'a, str>>,
    pub timestamp: Option<Value>,
    #[serde(borrow)]
    pub data: Option<PinData<'a>>,
}

#[derive(Deserialize)]
pub(super) struct PinData<'a> {
    #[serde(borrow)]
    pub action: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub provider: Option<Cow<'a, str>>,
    #[serde(rename = "sessionId", borrow)]
    pub session_id: Option<Cow<'a, str>>,
    #[serde(rename = "credentialId")]
    pub credential: Option<Value>,
    #[serde(borrow)]
    pub reason: Option<Cow<'a, str>>,
    pub at: Option<Value>,
}

impl ApiKeyHistory {
    pub fn observe_metadata(&mut self, catalog: &AccountCatalog, entry: &Metadata<'_>) -> bool {
        if entry.kind != "custom" || entry.custom_type.as_deref() != Some(ENTRY) {
            return false;
        }
        let Some(data) = &entry.data else {
            return false;
        };
        self.observe(
            catalog,
            entry.id.as_deref(),
            entry.timestamp.as_ref(),
            data.provider.as_deref(),
            data.session_id.as_deref(),
            data.action.as_deref(),
            data.credential.as_ref(),
            data.reason.as_deref(),
            data.at.as_ref(),
        )
    }

    pub fn observe_value(&mut self, catalog: &AccountCatalog, entry: &Value) -> bool {
        if entry.get("type").and_then(Value::as_str) != Some("custom")
            || entry.get("customType").and_then(Value::as_str) != Some(ENTRY)
        {
            return false;
        }
        let Some(data) = entry.get("data") else {
            return false;
        };
        self.observe(
            catalog,
            entry.get("id").and_then(Value::as_str),
            entry.get("timestamp"),
            data.get("provider").and_then(Value::as_str),
            data.get("sessionId").and_then(Value::as_str),
            data.get("action").and_then(Value::as_str),
            data.get("credentialId"),
            data.get("reason").and_then(Value::as_str),
            data.get("at"),
        )
    }

    fn observe(
        &mut self,
        catalog: &AccountCatalog,
        record_id: Option<&str>,
        timestamp: Option<&Value>,
        provider: Option<&str>,
        session: Option<&str>,
        action: Option<&str>,
        credential: Option<&Value>,
        reason: Option<&str>,
        at: Option<&Value>,
    ) -> bool {
        if provider != Some("opencode-go") {
            return false;
        }
        let Some(session) = session.filter(|session| !session.is_empty()) else {
            return false;
        };
        let Some(at) = at
            .and_then(timestamp_ms)
            .or_else(|| timestamp.and_then(timestamp_ms))
        else {
            return false;
        };
        let credential = match action {
            Some("pin") => {
                let Some(id) = credential.and_then(credential_id) else {
                    return false;
                };
                Some(id)
            }
            Some("release") => None,
            _ => return false,
        };
        let change = Change {
            at,
            record_id: record_id.map(str::to_owned),
            credential,
            reason: reason
                .filter(|reason| !reason.is_empty())
                .map(str::to_owned),
        };
        let namespace = catalog.namespace();
        if !self.scopes.contains_key(namespace) {
            self.scopes.insert(namespace.to_path_buf(), HashMap::new());
        }
        let sessions = self.scopes.get_mut(namespace).expect("scope was inserted");
        if !sessions.contains_key(session) {
            sessions.insert(session.to_owned(), Vec::new());
        }
        let timeline = sessions.get_mut(session).expect("session was inserted");
        if timeline.iter().any(|existing| existing == &change) {
            return false;
        }
        let position = timeline.partition_point(|existing| existing.at <= at);
        timeline.insert(position, change);
        if !self.dirty.contains(session) {
            self.dirty.insert(session.to_owned());
        }
        true
    }

    pub fn resolve(
        &self,
        catalog: &AccountCatalog,
        session: &str,
        at: i64,
    ) -> Option<(Option<AccountIdentity>, Option<String>)> {
        let timeline = self.scopes.get(catalog.namespace())?.get(session)?;
        let end = timeline.partition_point(|change| change.at <= at);
        let Some(position) = end.checked_sub(1) else {
            return Some((None, None));
        };
        let current = &timeline[position];
        let Some(credential) = &current.credential else {
            return Some((None, None));
        };
        // A later store replacement makes current row IDs unsafe for that old era.
        if timeline[end..]
            .iter()
            .any(|change| change.reason.as_deref() == Some("store-replaced"))
        {
            return Some((None, None));
        }
        let reason = current
            .reason
            .as_deref()
            .or_else(|| {
                let previous = position.checked_sub(1).map(|index| &timeline[index]);
                match previous {
                    Some(previous) if previous.credential.is_none() => previous.reason.as_deref(),
                    Some(previous) if previous.credential != current.credential => {
                        Some("usage-ranking")
                    }
                    _ => Some("initial"),
                }
            })
            .unwrap_or("initial");
        Some((
            catalog.resolve_credential_id("opencode-go", credential),
            Some(reason.to_owned()),
        ))
    }

    pub fn unique_namespace(&self, session: &str) -> Option<&Path> {
        let mut matched = None;
        for (namespace, sessions) in &self.scopes {
            if sessions.contains_key(session) {
                if matched.is_some() {
                    return None;
                }
                matched = Some(namespace.as_path());
            }
        }
        matched
    }

    pub fn dirty_sessions(&self) -> &HashSet<String> {
        &self.dirty
    }
    pub fn clear_dirty(&mut self) {
        self.dirty.clear();
    }
}
