//! Schema bootstrap, idempotent event writes and watermark persistence.
//!
//! WAL plus `synchronous=NORMAL` keeps the read-only consumer (ai-monitor) from
//! blocking the writer while making a crash lose at most the last transaction
//! (which the primary key absorbs on replay).

use crate::{cost::Priced, event::ParsedEvent};
use chrono::{Local, Timelike, TimeZone};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

pub const SCHEMA: &str = include_str!("../migrations/schema.sql");

/// Open (creating if needed) the collector database and apply the schema.
pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let connection = Connection::open(path)?;
    initialize(&connection)?;
    Ok(connection)
}

/// Open an existing database read-only without creating it.
pub fn open_readonly(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
}

/// Apply the schema. Idempotent: every statement uses `IF NOT EXISTS`.
///
/// The batch also creates `idx_usage_provider_time`, which cannot run on a
/// database created before that column existed, so the column is added first.
/// An empty column set means the table is created by the batch itself.
pub fn initialize(connection: &Connection) -> rusqlite::Result<()> {
    let columns: Vec<String> = {
        let mut statement = connection.prepare("PRAGMA table_info(usage_event)")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
        rows.filter_map(Result::ok).collect()
    };
    if !columns.is_empty() {
        for (name, sql_type) in [
            ("provider", "TEXT"),
            ("session_id", "TEXT"),
            ("started_at", "INTEGER"),
            ("completed_at", "INTEGER"),
            ("duration_ms", "INTEGER"),
            ("account_key", "TEXT"),
            ("account_label", "TEXT"),
            ("account_source", "TEXT"),
        ] {
            if !columns.iter().any(|column| column == name) {
                connection.execute(
                    &format!("ALTER TABLE usage_event ADD COLUMN {name} {sql_type}"),
                    [],
                )?;
            }
        }
    }
    connection.execute_batch(SCHEMA)
}

/// Insert one batch of events, absorbing replays through the primary key.
///
/// Returns how many rows were actually new. The caller must persist the
/// watermark only after this returns, so a crash replays the batch rather than
/// losing it.
pub fn insert_events(
    connection: &mut Connection,
    source: &str,
    events: &[(ParsedEvent, Priced)],
    now_ms: i64,
) -> rusqlite::Result<usize> {
    let transaction = connection.transaction()?;
    let mut inserted = 0usize;
    let mut newly_unresolved: Vec<&ParsedEvent> = vec![];
    {
        let mut statement = transaction.prepare(
            "INSERT OR IGNORE INTO usage_event(
                 source, event_id, model, model_source, provider,
                 session_id, started_at, completed_at, duration_ms,
                 account_key, account_label, account_source,
                 input_total, cache_read, cache_write, output_total, reasoning,
                 cost_usd, occurred_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                     ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
        )?;
        for (event, priced) in events {
            let (usage, adjusted) = event.usage.clamped();
            if adjusted {
                eprintln!(
                    "[warn] {source} {} usage violated the canonical invariants; clamped",
                    event.event_id
                );
            }
            let cost = match priced {
                Priced::Cost(value) => Some(*value),
                // `ignore = 1` and unpriced models both store NULL. The two are
                // told apart at query time through the model_alias join, which is
                // why `ignore` must stay in the alias table rather than in the
                // event row.
                Priced::Ignored | Priced::Unresolved => None,
            };
            let inserted_row = statement.execute(rusqlite::params![
                source,
                event.event_id,
                event.model,
                event.model_source.map(|value| value.as_str()),
                event.provider,
                event.session_id,
                event.started_at,
                event.completed_at,
                event.duration_ms,
                event.account_key,
                event.account_label,
                event.account_source,
                usage.input_total,
                usage.cache_read,
                usage.cache_write,
                usage.output_total,
                usage.reasoning,
                cost,
                event.occurred_at,
            ])?;
            // Only a genuinely new row reaches the unresolved ledger. The
            // opencode replay window re-offers the same rows every round;
            // counting replays would inflate `hit_count` once per minute and
            // drown the signal that ranks models needing an alias.
            if inserted_row == 1 && *priced == Priced::Unresolved {
                newly_unresolved.push(event);
            }
            inserted += inserted_row;
        }
    }
    {
        let mut statement = transaction.prepare(
            "INSERT INTO unresolved_model(source, raw_model, hit_count, first_seen, last_seen)
             VALUES (?1, ?2, 1, ?3, ?3)
             ON CONFLICT(source, raw_model) DO UPDATE SET
                 hit_count = hit_count + 1,
                 last_seen = excluded.last_seen",
        )?;
        for event in newly_unresolved.iter() {
            let Some(model) = &event.model else { continue };
            statement.execute(rusqlite::params![source, model, now_ms])?;
        }
    }
    transaction.commit()?;
    Ok(inserted)
}

/// Read and write one source's watermark.
pub fn offset(connection: &Connection, kind: &str) -> rusqlite::Result<Option<String>> {
    connection
        .query_row(
            "SELECT cursor FROM collect_offset WHERE kind = ?1",
            [kind],
            |row| row.get::<_, String>(0),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
}

/// Persist a source's watermark. Called in the same round as, and after, the
/// corresponding inserts.
pub fn save_offset(connection: &Connection, kind: &str, cursor: &str) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO collect_offset(kind, cursor) VALUES (?1, ?2)
         ON CONFLICT(kind) DO UPDATE SET cursor = excluded.cursor",
        rusqlite::params![kind, cursor],
    )?;
    Ok(())
}

pub const HOURLY_ROLLUP_KIND: &str = "hourly-rollup";

pub fn compute_current_hour_start_ms(now_ms: i64) -> Option<i64> {
    let dt = match Local.timestamp_millis_opt(now_ms) {
        chrono::LocalResult::Single(dt) => dt,
        chrono::LocalResult::Ambiguous(dt, _) => dt,
        chrono::LocalResult::None => return None,
    };
    let hour_start = dt
        .with_minute(0)?
        .with_second(0)?
        .with_nanosecond(0)?;
    Some(hour_start.timestamp_millis())
}

/// Archive past closed hours into `usage_hourly` up to `current_hour_start_ms`.
///
/// Current in-progress hour (`occurred_at >= current_hour_start_ms`) is excluded.
/// Closed past hours are never re-scanned or modified once archived.
/// Watermark is persisted in `collect_offset` under `hourly-rollup`.
pub fn rollup_closed_hours(connection: &mut Connection, now_ms: i64) -> rusqlite::Result<usize> {
    let current_hour_start_ms = match compute_current_hour_start_ms(now_ms) {
        Some(ms) => ms,
        None => return Ok(0),
    };

    let cursor: i64 = offset(connection, HOURLY_ROLLUP_KIND)?
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);

    if cursor >= current_hour_start_ms {
        return Ok(0);
    }

    let tx = connection.transaction()?;

    let distinct_hours: usize = {
        let mut stmt = tx.prepare(
            "SELECT COUNT(DISTINCT day || '#' || hour) FROM (
                SELECT date(occurred_at / 1000, 'unixepoch', 'localtime') AS day,
                       CAST(strftime('%H', occurred_at / 1000, 'unixepoch', 'localtime') AS INTEGER) AS hour
                FROM usage_event
                WHERE occurred_at >= ?1 AND occurred_at < ?2
            )",
        )?;
        stmt.query_row(rusqlite::params![cursor, current_hour_start_ms], |r| {
            r.get(0)
        })?
    };

    if distinct_hours > 0 {
        tx.execute(
            "INSERT INTO usage_hourly(day, hour, model, input_total, output_total, tokens, cache_read, cost_usd, events)
             SELECT
                 date(e.occurred_at / 1000, 'unixepoch', 'localtime') AS day,
                 CAST(strftime('%H', e.occurred_at / 1000, 'unixepoch', 'localtime') AS INTEGER) AS hour,
                 '' AS model,
                 SUM(e.input_total) AS input_total,
                 SUM(e.output_total) AS output_total,
                 SUM(e.input_total + e.output_total) AS tokens,
                 SUM(e.cache_read) AS cache_read,
                 SUM(e.cost_usd) AS cost_usd,
                 COUNT(*) AS events
             FROM usage_event e
             WHERE e.occurred_at >= ?1 AND e.occurred_at < ?2
             GROUP BY 1, 2
             UNION ALL
             SELECT
                 date(e.occurred_at / 1000, 'unixepoch', 'localtime') AS day,
                 CAST(strftime('%H', e.occurred_at / 1000, 'unixepoch', 'localtime') AS INTEGER) AS hour,
                 CASE WHEN COALESCE(a.model_id, e.model, '') = '' THEN '__unknown__' ELSE COALESCE(a.model_id, e.model) END AS model,
                 SUM(e.input_total) AS input_total,
                 SUM(e.output_total) AS output_total,
                 SUM(e.input_total + e.output_total) AS tokens,
                 SUM(e.cache_read) AS cache_read,
                 SUM(e.cost_usd) AS cost_usd,
                 COUNT(*) AS events
             FROM usage_event e
             LEFT JOIN model_alias a ON a.raw_model = e.model
             WHERE e.occurred_at >= ?1 AND e.occurred_at < ?2
             GROUP BY 1, 2, 3
             ON CONFLICT(day, hour, model) DO UPDATE SET
                 input_total = usage_hourly.input_total + excluded.input_total,
                 output_total = usage_hourly.output_total + excluded.output_total,
                 tokens = usage_hourly.tokens + excluded.tokens,
                 cache_read = usage_hourly.cache_read + excluded.cache_read,
                 cost_usd = CASE
                     WHEN usage_hourly.cost_usd IS NULL AND excluded.cost_usd IS NULL THEN NULL
                     ELSE COALESCE(usage_hourly.cost_usd, 0.0) + COALESCE(excluded.cost_usd, 0.0)
                 END,
                 events = usage_hourly.events + excluded.events",
            rusqlite::params![cursor, current_hour_start_ms],
        )?;
    }

    tx.execute(
        "INSERT INTO collect_offset(kind, cursor) VALUES (?1, ?2)
         ON CONFLICT(kind) DO UPDATE SET cursor = excluded.cursor",
        rusqlite::params![HOURLY_ROLLUP_KIND, current_hour_start_ms.to_string()],
    )?;

    tx.commit()?;
    Ok(distinct_hours)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{ModelSource, UsageEvent};

    fn event(id: &str, at: i64) -> ParsedEvent {
        ParsedEvent {
            event_id: id.to_owned(),
            usage: UsageEvent {
                input_total: 100,
                cache_read: 40,
                cache_write: 0,
                output_total: 10,
                reasoning: 3,
            },
            model: Some("opencode/muse-spark-1.3-contributor-free".into()),
            model_source: Some(ModelSource::Event),
            provider: Some("opencode".into()),
            session_id: Some("session-1".into()),
            started_at: Some(at),
            completed_at: Some(at + 10),
            duration_ms: Some(10),
            account_key: None,
            account_label: None,
            account_source: None,
            occurred_at: at,
        }
    }

    fn memory() -> Connection {
        let connection = Connection::open_in_memory().expect("memory db");
        initialize(&connection).expect("schema");
        connection
    }

    #[test]
    fn replaying_a_batch_is_absorbed_by_the_primary_key() {
        let mut connection = memory();
        let batch = [
            (event("1", 10), Priced::Ignored),
            (event("2", 20), Priced::Ignored),
        ];
        assert_eq!(
            insert_events(&mut connection, "opencode", &batch, 0).expect("insert"),
            2
        );
        // The opencode replay window re-reads these rows every round.
        assert_eq!(
            insert_events(&mut connection, "opencode", &batch, 0).expect("replay"),
            0
        );
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM usage_event", [], |row| row.get(0))
            .expect("count");
        assert_eq!(count, 2, "a replayed window must not double count");
    }

    #[test]
    fn a_replayed_batch_does_not_inflate_the_unresolved_ledger() {
        let mut connection = memory();
        let batch = [(event("1", 10), Priced::Unresolved)];
        assert_eq!(
            insert_events(&mut connection, "opencode", &batch, 1_000).expect("insert"),
            1
        );
        // The opencode replay window re-offers the same row every round; each
        // replay must leave `hit_count` untouched.
        for now in [2_000, 3_000, 4_000] {
            assert_eq!(
                insert_events(&mut connection, "opencode", &batch, now).expect("replay"),
                0
            );
        }
        let (hit_count, last_seen): (i64, i64) = connection
            .query_row(
                "SELECT hit_count, last_seen FROM unresolved_model WHERE source='opencode'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("ledger row");
        assert_eq!(hit_count, 1, "replays must not inflate hit_count");
        assert_eq!(last_seen, 1_000, "replays must not bump last_seen");
    }

    #[test]
    fn the_same_event_id_under_two_sources_stays_two_rows() {
        let mut connection = memory();
        insert_events(
            &mut connection,
            "omp",
            &[(event("1", 10), Priced::Ignored)],
            0,
        )
        .expect("omp insert");
        insert_events(
            &mut connection,
            "grok",
            &[(event("1", 10), Priced::Ignored)],
            0,
        )
        .expect("grok insert");
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM usage_event", [], |row| row.get(0))
            .expect("count");
        assert_eq!(count, 2);
    }

    #[test]
    fn only_unpriced_events_reach_the_unresolved_ledger() {
        let mut connection = memory();
        insert_events(
            &mut connection,
            "omp",
            &[
                (event("1", 10), Priced::Unresolved),
                (event("2", 20), Priced::Cost(0.5)),
                (event("3", 30), Priced::Ignored),
            ],
            1_000,
        )
        .expect("insert");
        let rows: Vec<(String, i64)> = {
            let mut statement = connection
                .prepare("SELECT raw_model, hit_count FROM unresolved_model")
                .expect("prepare");
            let mapped = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .expect("query");
            mapped.filter_map(Result::ok).collect()
        };
        assert_eq!(
            rows,
            vec![("opencode/muse-spark-1.3-contributor-free".to_string(), 1)]
        );
    }

    #[test]
    fn watermarks_round_trip_and_are_absent_before_the_first_round() {
        let connection = memory();
        assert_eq!(offset(&connection, "omp").expect("read"), None);
        save_offset(&connection, "omp", "{\"offsets\":{\"/a\":12}}").expect("save");
        assert_eq!(
            offset(&connection, "omp").expect("read").as_deref(),
            Some("{\"offsets\":{\"/a\":12}}")
        );
        save_offset(&connection, "omp", "{\"offsets\":{\"/a\":42}}").expect("update");
        assert_eq!(
            offset(&connection, "omp").expect("read").as_deref(),
            Some("{\"offsets\":{\"/a\":42}}")
        );
    }

    #[test]
    fn the_schema_is_idempotent() {
        let connection = memory();
        initialize(&connection).expect("second apply");
        initialize(&connection).expect("third apply");
    }

    #[test]
    fn test_rollup_closed_hours_only_archives_past_hours() {
        let mut connection = memory();
        let now_ms = 1_774_971_000_000;
        let current_hour_start = compute_current_hour_start_ms(now_ms).unwrap();

        let yesterday_at = current_hour_start - 24 * 3600 * 1000 + 10_000;
        let earlier_today_at = current_hour_start - 2 * 3600 * 1000 + 10_000;
        let current_hour_at = current_hour_start + 10_000;
        let later_current_at = current_hour_start + 25 * 60 * 1000;

        let batch = [
            (event("ev-yesterday", yesterday_at), Priced::Cost(0.1)),
            (event("ev-earlier", earlier_today_at), Priced::Cost(0.2)),
            (event("ev-current", current_hour_at), Priced::Cost(0.3)),
            (event("ev-later", later_current_at), Priced::Cost(0.4)),
        ];
        insert_events(&mut connection, "omp", &batch, now_ms).expect("insert");

        let affected = rollup_closed_hours(&mut connection, now_ms).expect("rollup");
        assert_eq!(affected, 2, "must archive exactly the 2 closed hours");

        let watermark = offset(&connection, HOURLY_ROLLUP_KIND).expect("offset").expect("present");
        assert_eq!(watermark, current_hour_start.to_string());

        let rows: Vec<(String, i64, String, i64, i64, Option<f64>, i64)> = {
            let mut stmt = connection
                .prepare("SELECT day, hour, model, tokens, events, cost_usd, input_total FROM usage_hourly ORDER BY day, hour, model")
                .expect("prepare");
            let mapped = stmt
                .query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                })
                .expect("query");
            mapped.filter_map(Result::ok).collect()
        };

        assert_eq!(rows.len(), 4, "expected 2 hours x 2 entries (all-models + per-model)");

        for (_day, _hour, model, tokens, events, cost, input) in &rows {
            assert_eq!(*tokens, 110);
            assert_eq!(*input, 100);
            assert_eq!(*events, 1);
            if model.is_empty() {
                let c = cost.unwrap();
                assert!((c - 0.1).abs() < 1e-4 || (c - 0.2).abs() < 1e-4);
            }
        }

        let second_run = rollup_closed_hours(&mut connection, now_ms).expect("second rollup");
        assert_eq!(second_run, 0, "must be idempotent when within the same hour");

        let next_hour_now = now_ms + 3600 * 1000;
        let third_run = rollup_closed_hours(&mut connection, next_hour_now).expect("third rollup");
        assert_eq!(third_run, 1, "previous current hour should now be archived as 1 closed hour");

        let total_hourly_rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM usage_hourly", [], |r| r.get(0))
            .expect("count");
        assert_eq!(total_hourly_rows, 6, "now 3 hours x 2 entries");

        let current_hour_summary_events: i64 = connection
            .query_row(
                "SELECT events FROM usage_hourly WHERE model = '' ORDER BY day DESC, hour DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .expect("events");
        assert_eq!(current_hour_summary_events, 2);
    }
}
