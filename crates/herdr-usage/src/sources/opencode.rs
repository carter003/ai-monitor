//! opencode session database reader (`~/.local/share/opencode/opencode.db`).
//!
//! Unlike the other three sources this is not a log file but a sqlite database
//! whose `message` table holds `data` as JSON. It is opened **read-only** and is
//! never written by the collector.
//!
//! Measured upstream facts (plan §2.3, §2.4, §2.7):
//!
//! * `message` is a rowid table (1..160821). Rows are ordered by `rowid`, and the
//!   last 3000 rows matched `time_created` ordering exactly (0 violations), so a
//!   rowid cursor is safe.
//! * Assistant rows carry `tokens.{input, output, reasoning, cache.read, cache.write}`
//!   plus `modelID` and `providerID`.
//! * **Rows are created before they are filled.** Measured `time_updated −
//!   time_created` percentiles: p50≈0h, p90≈390h, p99≈905h, max≈928h; within the
//!   last 2000/10000 rows about 3.6% / 2.3% are still backfilled 60 seconds after
//!   creation (max lag 0.5h). A monotone `rowid` cursor would therefore lose those
//!   rows forever, which is why the caller replays an overlapping window and this
//!   module emits stable, replay-safe ids.
//! * 807 assistant rows are still all-zero (unfinished). They are skipped: storing
//!   them would both pollute the sums and (because of the primary key) block the
//!   real values from landing once the row is filled.

use crate::event::{normalize_opencode, ModelSource, OcCache, OcTokens, ParsedEvent};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

/// Rows replayed on every pass. Measured tail of 2000 rows reaches roughly 184h
/// back, far beyond the 0.5h worst-case backfill lag.
pub const REPLAY_WINDOW: i64 = 2000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawMessage {
    pub rowid: i64,
    pub data: String,
}

/// Open the opencode database read-only. Never creates the file.
pub fn open(path: &std::path::Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
}

/// Highest rowid currently in `message`, or 0 when the table is empty/missing.
pub fn max_rowid(connection: &Connection) -> i64 {
    connection
        .query_row("SELECT IFNULL(MAX(rowid), 0) FROM message", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap_or(0)
}

/// The window's exclusive lower rowid bound for one round: never below the
/// baseline recorded at the first ever round (the file sources' "不回填历史"
/// rule — without it the first round would import the last `REPLAY_WINDOW`
/// rows of pre-collector history), yet still reaching back `REPLAY_WINDOW`
/// rows so a row filled in long after its creation is re-read.
pub fn replay_floor(baseline: i64, max: i64) -> i64 {
    baseline.max(max.saturating_sub(REPLAY_WINDOW))
}

/// The baseline rowid in the `{"max_rowid":N}` cursor. `None` — absent or
/// unparsable cursor — (re)baselines the source on the caller's next write.
pub fn baseline_from_cursor(cursor: &str) -> Option<i64> {
    serde_json::from_str::<Value>(cursor)
        .ok()?
        .get("max_rowid")?
        .as_i64()
}

/// Read the replay window: every row with `rowid > floor`, in rowid order so
/// the caller processes them deterministically.
pub fn read_window(connection: &Connection, floor: i64) -> rusqlite::Result<Vec<RawMessage>> {
    let mut statement =
        connection.prepare("SELECT rowid, data FROM message WHERE rowid > ?1 ORDER BY rowid")?;
    let rows = statement.query_map([floor], |row| {
        Ok(RawMessage {
            rowid: row.get(0)?,
            data: row.get(1)?,
        })
    })?;
    rows.collect()
}

/// Convert one `message.data` blob into a usage event.
///
/// Returns `None` for non-assistant rows, rows whose `tokens` are absent or still
/// all-zero, and rows without a provider/model pair.
pub fn parse_message(row: &RawMessage) -> Option<ParsedEvent> {
    let value: Value = serde_json::from_str(&row.data).ok()?;
    if value.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let tokens = value.get("tokens")?;
    if tokens.is_null() {
        return None;
    }
    let parsed = OcTokens {
        input: int(tokens, "input"),
        output: int(tokens, "output"),
        reasoning: int(tokens, "reasoning"),
        cache: OcCache {
            read: tokens
                .get("cache")
                .map(|cache| int(cache, "read"))
                .unwrap_or(0),
            write: tokens
                .get("cache")
                .map(|cache| int(cache, "write"))
                .unwrap_or(0),
        },
    };
    // An unfinished row has no usage at all. Skipping (rather than storing a zero
    // row) is what lets the replay window insert the real numbers later.
    if parsed.input == 0
        && parsed.output == 0
        && parsed.reasoning == 0
        && parsed.cache.read == 0
        && parsed.cache.write == 0
    {
        return None;
    }
    let occurred_at = value
        .pointer("/time/created")
        .and_then(Value::as_i64)
        .or_else(|| value.pointer("/time/completed").and_then(Value::as_i64))?;

    let provider = value
        .get("providerID")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty());
    let model_id = value
        .get("modelID")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty());
    // Stored as `providerID/modelID` so the four sources share one alias key.
    // Measured: two modelIDs appear under multiple providers
    // (`gemini-3-pro-preview`, `gemini-3.1-flash-lite-preview`), so storing the
    // bare id would merge distinct models.
    let model = match (provider, model_id) {
        (Some(provider), Some(model)) => Some(format!("{provider}/{model}")),
        (None, Some(model)) => Some(model.to_owned()),
        _ => None,
    };

    Some(ParsedEvent {
        // rowid is the stable identity and survives the replay window via
        // `INSERT OR IGNORE` on the (source, event_id) primary key.
        event_id: row.rowid.to_string(),
        usage: normalize_opencode(&parsed),
        model_source: model.as_ref().map(|_| ModelSource::Event),
        model,
        occurred_at,
    })
}

fn int(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(rowid: i64, data: &str) -> RawMessage {
        RawMessage {
            rowid,
            data: data.to_owned(),
        }
    }

    const ASSISTANT: &str = r#"{"role":"assistant","modelID":"muse-spark-1.3-contributor-free","providerID":"opencode","tokens":{"total":129352,"input":411,"output":60,"reasoning":0,"cache":{"write":0,"read":128881}},"cost":0,"time":{"created":1788519452477,"completed":1788519456306}}"#;

    #[test]
    fn provider_and_model_are_joined_into_the_alias_key() {
        let event = parse_message(&raw(160821, ASSISTANT)).expect("assistant row");
        assert_eq!(
            event.model.as_deref(),
            Some("opencode/muse-spark-1.3-contributor-free")
        );
        assert_eq!(event.model_source, Some(ModelSource::Event));
        assert_eq!(event.event_id, "160821");
        assert_eq!(event.occurred_at, 1_788_519_452_477);
    }

    #[test]
    fn tokens_are_normalized_to_gross_input_and_reasoning_inclusive_output() {
        let event = parse_message(&raw(160821, ASSISTANT)).expect("assistant row");
        assert_eq!(event.usage.input_total, 129_292, "411 net + 128881 cache");
        assert_eq!(event.usage.cache_read, 128_881);
        assert_eq!(event.usage.output_total, 60);
        assert_eq!(event.usage.reasoning, 0);
    }

    #[test]
    fn independent_reasoning_is_folded_into_output() {
        // A row where reasoning exceeds the declared output, as measured on
        // 27888 of 147799 rows.
        let text = r#"{"role":"assistant","modelID":"m","providerID":"p","tokens":{"input":10,"output":20,"reasoning":900,"cache":{"read":5,"write":7}},"time":{"created":1700000000000}}"#;
        let event = parse_message(&raw(7, text)).expect("assistant row");
        assert_eq!(event.usage.output_total, 920);
        assert_eq!(event.usage.reasoning, 900);
        assert_eq!(event.usage.cache_write, 7);
        assert_eq!(event.usage.input_total, 15);
    }

    #[test]
    fn all_zero_unfinished_rows_are_skipped_so_the_window_can_fill_them_later() {
        let zero = r#"{"role":"assistant","modelID":"m","providerID":"p","tokens":{"total":0,"input":0,"output":0,"reasoning":0,"cache":{"write":0,"read":0}},"time":{"created":1700000000000}}"#;
        assert!(parse_message(&raw(9, zero)).is_none());
        let missing = r#"{"role":"assistant","modelID":"m","providerID":"p","time":{"created":1700000000000}}"#;
        assert!(parse_message(&raw(10, missing)).is_none());
    }

    #[test]
    fn user_rows_and_malformed_data_yield_nothing() {
        assert!(parse_message(&raw(1, r#"{"role":"user","text":"hi"}"#)).is_none());
        assert!(parse_message(&raw(2, "not json")).is_none());
    }

    #[test]
    fn the_replay_window_covers_rows_that_were_backfilled_late() {
        let connection = Connection::open_in_memory().expect("memory db");
        connection
            .execute(
                "CREATE TABLE message(rowid INTEGER PRIMARY KEY, data TEXT)",
                [],
            )
            .expect("schema");
        // A row written empty at creation time and filled in afterwards, exactly
        // like the measured backfill case.
        connection
            .execute(
                "INSERT INTO message(rowid, data) VALUES (1, '{\"role\":\"assistant\",\"tokens\":{\"input\":0,\"output\":0,\"reasoning\":0,\"cache\":{\"read\":0,\"write\":0}},\"time\":{\"created\":1}}')",
                [],
            )
            .expect("insert empty row");
        for rowid in 2..=(REPLAY_WINDOW) {
            connection
                .execute(
                    "INSERT INTO message(rowid, data) VALUES (?1, ?2)",
                    rusqlite::params![rowid, ASSISTANT],
                )
                .expect("insert filler");
        }
        // The late backfill lands on row 1, which a monotone cursor would have
        // passed long ago.
        connection
            .execute(
                "UPDATE message SET data = ?1 WHERE rowid = 1",
                rusqlite::params![ASSISTANT.replace("\"input\":411", "\"input\":999")],
            )
            .expect("backfill row");

        let max = max_rowid(&connection);
        assert_eq!(max, REPLAY_WINDOW);
        let window = read_window(&connection, replay_floor(0, max)).expect("window read");
        // The floor is 0, so row 1 remains in scope and the backfilled values
        // are seen on this pass.
        let recovered = window
            .iter()
            .find(|row| row.rowid == 1)
            .expect("the backfilled row must remain inside the replay window");
        let event = parse_message(recovered).expect("backfilled row now has usage");
        assert_eq!(event.usage.input_total, 999 + 128_881);
        assert_eq!(event.event_id, "1");
    }

    #[test]
    fn the_replay_floor_never_drops_below_the_baseline() {
        assert_eq!(replay_floor(700, 900), 700, "baseline still in force");
        assert_eq!(
            replay_floor(700, 5000),
            3000,
            "window reaches back REPLAY_WINDOW rows"
        );
        assert_eq!(
            replay_floor(700, 1000),
            700,
            "a shrunken table cannot expose pre-baseline rows"
        );
    }

    #[test]
    fn the_baseline_is_recovered_from_the_cursor() {
        assert_eq!(
            baseline_from_cursor("{\"max_rowid\":158821}"),
            Some(158_821)
        );
        assert_eq!(baseline_from_cursor("{}"), None);
        assert_eq!(baseline_from_cursor("not json"), None);
    }

    #[test]
    fn a_row_older_than_the_window_is_outside_it() {
        let connection = Connection::open_in_memory().expect("memory db");
        connection
            .execute(
                "CREATE TABLE message(rowid INTEGER PRIMARY KEY, data TEXT)",
                [],
            )
            .expect("schema");
        connection
            .execute(
                "INSERT INTO message(rowid, data) VALUES (1, ?1)",
                rusqlite::params![ASSISTANT],
            )
            .expect("insert old row");
        for rowid in 2..=(REPLAY_WINDOW * 2) {
            connection
                .execute(
                    "INSERT INTO message(rowid, data) VALUES (?1, ?2)",
                    rusqlite::params![rowid, ASSISTANT],
                )
                .expect("insert filler");
        }
        let window =
            read_window(&connection, replay_floor(0, max_rowid(&connection))).expect("window read");
        assert_eq!(window.len(), REPLAY_WINDOW as usize);
        assert!(window.iter().all(|row| row.rowid > REPLAY_WINDOW));
    }
}
