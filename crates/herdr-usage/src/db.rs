//! Schema bootstrap, idempotent event writes and watermark persistence.
//!
//! WAL plus `synchronous=NORMAL` keeps the read-only consumer (ai-monitor) from
//! blocking the writer while making a crash lose at most the last transaction
//! (which the primary key absorbs on replay).

use crate::{cost::Priced, event::ParsedEvent};
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
    if !columns.is_empty() && !columns.iter().any(|name| name == "provider") {
        connection.execute("ALTER TABLE usage_event ADD COLUMN provider TEXT", [])?;
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
                 input_total, cache_read, cache_write, output_total, reasoning,
                 cost_usd, occurred_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
}
