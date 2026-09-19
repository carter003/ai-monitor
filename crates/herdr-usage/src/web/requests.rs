//! Request-level latency and account-routing report.
//!
//! This intentionally reads only normalized metadata. Prompt, response and
//! tool content never enter `usage.db` and therefore cannot leak through here.

use chrono::{Local, TimeZone};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::path::Path;

const RECENT_REQUEST_LIMIT: usize = 3_000;

#[derive(Debug, Serialize)]
pub struct RequestRow {
    pub event_id: String,
    /// Collector source/client (`omp`, `opencode`, ...). This already exists in
    /// SQLite as `usage_event.source`; the API name makes its UI role explicit.
    pub client: String,
    pub session_id: String,
    pub provider: String,
    pub model: Option<String>,
    pub account_key: Option<String>,
    pub account: Option<String>,
    pub account_source: Option<String>,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub local_time: String,
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct CrossAccountSession {
    pub client: String,
    pub session_id: String,
    pub provider: String,
    pub accounts: Vec<String>,
    pub requests: usize,
    pub first_at: i64,
    pub last_at: i64,
}

#[derive(Debug, Serialize)]
pub struct RequestReport {
    pub timezone: String,
    pub requests: usize,
    pub resolved: usize,
    pub average_duration_ms: Option<i64>,
    pub recent_limit: usize,
    pub cross_account_sessions: Vec<CrossAccountSession>,
    pub recent: Vec<RequestRow>,
}

pub fn load(path: &Path) -> Result<RequestReport, String> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| "无法读取统计数据库，请确认 collector 已运行".to_string())?;
    connection
        .busy_timeout(std::time::Duration::from_secs(3))
        .map_err(|error| error.to_string())?;
    report(&connection)
}

fn report(connection: &Connection) -> Result<RequestReport, String> {
    let (requests, resolved, average_duration_ms) = connection
        .query_row(
            "SELECT COUNT(*), COUNT(account_key), CAST(AVG(duration_ms) AS INTEGER)
             FROM usage_event
             WHERE source IN ('omp', 'codex', 'grok', 'opencode')
               AND session_id IS NOT NULL AND session_id <> ''",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)? as usize,
                    row.get::<_, i64>(1)? as usize,
                    row.get(2)?,
                ))
            },
        )
        .map_err(|error| error.to_string())?;

    let recent_sql = "SELECT event_id, source, COALESCE(session_id, ''),
                COALESCE(provider, source), model,
                account_key, account_label, account_source,
                COALESCE(started_at, occurred_at), completed_at, duration_ms
         FROM usage_event INDEXED BY idx_usage_recent_request
         WHERE source IN ('omp', 'codex', 'grok', 'opencode')
           AND session_id IS NOT NULL AND session_id <> ''
         ORDER BY COALESCE(started_at, occurred_at) DESC
         LIMIT ?1";
    let mut statement = connection
        .prepare(recent_sql)
        .or_else(|error| {
            // usage-web can start against a database that the updated collector
            // has not migrated yet. Preserve the page until the index exists.
            if error
                .to_string()
                .contains("no such index: idx_usage_recent_request")
            {
                connection.prepare(&recent_sql.replace(" INDEXED BY idx_usage_recent_request", ""))
            } else {
                Err(error)
            }
        })
        .map_err(|error| error.to_string())?;
    let mapped = statement
        .query_map([RECENT_REQUEST_LIMIT as i64], |row| {
            let started_at: i64 = row.get(8)?;
            Ok(RequestRow {
                event_id: row.get(0)?,
                client: row.get(1)?,
                session_id: row.get(2)?,
                provider: row.get(3)?,
                model: row.get(4)?,
                account_key: row.get(5)?,
                account: row.get(6)?,
                account_source: row.get(7)?,
                started_at,
                completed_at: row.get(9)?,
                local_time: Local
                    .timestamp_millis_opt(started_at)
                    .single()
                    .map(|time| time.format("%Y-%m-%d %H:%M:%S").to_string())
                    .unwrap_or_else(|| started_at.to_string()),
                duration_ms: row.get(10)?,
            })
        })
        .map_err(|error| error.to_string())?;
    let rows: Vec<RequestRow> = mapped
        .collect::<Result<_, _>>()
        .map_err(|error| error.to_string())?;

    // Aggregate once in SQLite. `account_rollup` produces one row per account,
    // then `HAVING COUNT(*) > 1` selects cross-account sessions. This avoids
    // loading all requests into Rust and avoids pairwise account comparisons.
    let mut cross_statement = connection
        .prepare(
            "WITH account_rollup AS (
                 SELECT source AS client,
                        session_id,
                        COALESCE(provider, source) AS provider,
                        account_key,
                        COALESCE(MAX(NULLIF(account_label, '')), account_key) AS account,
                        COUNT(*) AS requests,
                        MIN(COALESCE(started_at, occurred_at)) AS first_at,
                        MAX(COALESCE(started_at, occurred_at)) AS last_at
                 FROM usage_event
                 WHERE provider IN ('google-antigravity', 'opencode-go')
                   AND session_id IS NOT NULL AND session_id <> ''
                   AND account_key IS NOT NULL AND account_key <> ''
                 GROUP BY source, session_id, COALESCE(provider, source), account_key
             ),
             cross_sessions AS (
                 SELECT client, session_id, provider,
                        SUM(requests) AS requests,
                        MIN(first_at) AS first_at,
                        MAX(last_at) AS last_at
                 FROM account_rollup
                 GROUP BY client, session_id, provider
                 HAVING COUNT(*) > 1
             )
             SELECT cross_sessions.client,
                    cross_sessions.session_id,
                    cross_sessions.provider,
                    cross_sessions.requests,
                    cross_sessions.first_at,
                    cross_sessions.last_at,
                    account_rollup.account
             FROM cross_sessions
             JOIN account_rollup USING (client, session_id, provider)
             ORDER BY cross_sessions.last_at DESC,
                      cross_sessions.client,
                      cross_sessions.session_id,
                      cross_sessions.provider,
                      account_rollup.account_key",
        )
        .map_err(|error| error.to_string())?;
    let account_rows = cross_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)? as usize,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    let mut cross_account_sessions = Vec::<CrossAccountSession>::new();
    for row in account_rows {
        let (client, session_id, provider, requests, first_at, last_at, account) =
            row.map_err(|error| error.to_string())?;
        if let Some(existing) = cross_account_sessions.last_mut().filter(|existing| {
            existing.client == client
                && existing.session_id == session_id
                && existing.provider == provider
        }) {
            existing.accounts.push(account);
        } else {
            cross_account_sessions.push(CrossAccountSession {
                client,
                session_id,
                provider,
                accounts: vec![account],
                requests,
                first_at,
                last_at,
            });
        }
    }

    Ok(RequestReport {
        timezone: Local::now().format("%Z (UTC %:z)").to_string(),
        requests,
        resolved,
        average_duration_ms,
        recent_limit: RECENT_REQUEST_LIMIT,
        cross_account_sessions,
        recent: rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_request_query_uses_ordered_index() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../../migrations/schema.sql"))
            .unwrap();
        let plan: String = connection.query_row(
            "EXPLAIN QUERY PLAN SELECT event_id FROM usage_event INDEXED BY idx_usage_recent_request
             WHERE source IN ('omp', 'codex', 'grok', 'opencode')
               AND session_id IS NOT NULL AND session_id <> ''
             ORDER BY COALESCE(started_at, occurred_at) DESC LIMIT 3000",
            [], |row| row.get(3),
        ).unwrap();
        assert!(plan.contains("idx_usage_recent_request"), "{plan}");
        connection
            .execute_batch("DROP INDEX idx_usage_recent_request")
            .unwrap();
        assert!(
            report(&connection).is_ok(),
            "older databases remain readable before migration"
        );
    }

    #[test]
    fn one_session_can_be_reported_under_two_accounts() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(
            "CREATE TABLE usage_event(event_id TEXT, source TEXT, session_id TEXT, provider TEXT, model TEXT,
             account_key TEXT, account_label TEXT, account_source TEXT, started_at INTEGER,
             completed_at INTEGER, duration_ms INTEGER, occurred_at INTEGER);
             CREATE INDEX idx_usage_recent_request ON usage_event(COALESCE(started_at, occurred_at) DESC)
               WHERE source IN ('omp', 'codex', 'grok', 'opencode')
                 AND session_id IS NOT NULL AND session_id <> '';",
        ).unwrap();
        for (id, account, label, at) in [
            ("1", "a", "A", 1000),
            ("2", "a", "A", 1500),
            ("3", "b", "B", 2000),
        ] {
            connection.execute(
                "INSERT INTO usage_event VALUES (?1,'omp','s','opencode-go','m',?2,?3,'sticky_cache',?4,?4+10,10,?4)",
                rusqlite::params![id, account, label, at],
            ).unwrap();
        }
        connection
            .execute(
                "INSERT INTO usage_event VALUES
                 ('4','codex','codex-s',NULL,'gpt',NULL,NULL,NULL,3000,NULL,NULL,3000)",
                [],
            )
            .unwrap();
        let report = report(&connection).unwrap();
        assert_eq!(report.requests, 4);
        assert_eq!(report.recent[0].client, "codex");
        assert_eq!(report.recent[0].provider, "codex");
        assert_eq!(report.recent_limit, 3_000);
        assert_eq!(report.cross_account_sessions.len(), 1);
        assert_eq!(report.cross_account_sessions[0].accounts, ["A", "B"]);
        assert_eq!(report.cross_account_sessions[0].requests, 3);
    }

    #[test]
    fn summary_uses_all_rows_while_recent_is_bounded_in_sql() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(
            "CREATE TABLE usage_event(event_id TEXT, source TEXT, session_id TEXT, provider TEXT, model TEXT,
             account_key TEXT, account_label TEXT, account_source TEXT, started_at INTEGER,
             completed_at INTEGER, duration_ms INTEGER, occurred_at INTEGER);
             CREATE INDEX idx_usage_recent_request ON usage_event(COALESCE(started_at, occurred_at) DESC)
               WHERE source IN ('omp', 'codex', 'grok', 'opencode')
                 AND session_id IS NOT NULL AND session_id <> '';
             WITH RECURSIVE sequence(value) AS (
               SELECT 1 UNION ALL SELECT value + 1 FROM sequence WHERE value < 3005
             )
             INSERT INTO usage_event
             SELECT CAST(value AS TEXT), 'codex', 's-' || value, NULL, 'gpt',
                    NULL, NULL, NULL, value, value + 10, 10, value
             FROM sequence;",
        )
        .unwrap();

        let report = report(&connection).unwrap();
        assert_eq!(report.requests, 3_005);
        assert_eq!(report.recent.len(), RECENT_REQUEST_LIMIT);
        assert_eq!(report.average_duration_ms, Some(10));
    }
}
