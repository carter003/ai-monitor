//! Read-only queries over the herdr token-usage database.
//!
//! One owner for the aggregation SQL (§9.13) and the interval boundaries, so the
//! numbers the TUI shows cannot drift from the collector's accounting rules.
//!
//! The connection is opened read-only and kept for the life of the worker thread;
//! reopening it every frame would re-resolve the path and re-read the schema.
//! A missing file is not an error to panic on: `SQLITE_OPEN_READ_ONLY` returns
//! `Err` and the caller degrades to the "not connected" state.

use crate::model::{Bucketed, ModelUsage, UsageStats, UsageTotal};
use chrono::{Datelike, Duration as ChronoDuration, Local, NaiveDate, TimeZone};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

/// Everything the token page needs, read in one pass.
pub struct Reader {
    connection: Connection,
}

impl Reader {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        Ok(Self { connection })
    }

    /// Read every panel. Returns an error only when a query fails; an empty
    /// database is a valid, all-zero result.
    pub fn load(&self) -> rusqlite::Result<UsageStats> {
        let now = Local::now();
        let day_start = local_midnight(now, Period::Day);
        let week_start = local_midnight(now, Period::Week);
        let month_start = local_midnight(now, Period::Month);
        let year_start = local_midnight(now, Period::Year);
        let rolling_24h = now.timestamp_millis() - 24 * 3600 * 1000;

        let all_total = self.total(None)?;
        Ok(UsageStats {
            models: self.models(rolling_24h)?,
            month_models: self.models_with_limit(month_start, 10)?,
            hours: self.buckets(Bucket::QuarterHour, day_start)?,
            month: self.buckets(Bucket::SixHour, month_start)?,
            day_total: self.total(Some(day_start))?,
            week_total: self.total(Some(week_start))?,
            month_total: self.total(Some(month_start))?,
            year_total: self.total(Some(year_start))?,
            all_total,
            error: None,
        })
    }

    /// Interval totals: tokens and money since `since_ms`, or over the whole
    /// table when it is `None`.
    ///
    /// No join to `model_alias`: the totals count every event, including models
    /// the ranking filters out, and aliasing is the ranking's business.
    fn total(&self, since_ms: Option<i64>) -> rusqlite::Result<UsageTotal> {
        let (clause, parameter) = match since_ms {
            Some(value) => ("WHERE occurred_at >= ?1", Some(value)),
            None => ("", None),
        };
        let sql = format!(
            "SELECT SUM(input_total + output_total),
                    SUM(cost_usd)
             FROM usage_event
             {clause}"
        );
        let read = |row: &rusqlite::Row<'_>| {
            // `SUM` over an empty table is NULL, not 0, so both aggregates are
            // read as an Option.
            let tokens: Option<i64> = row.get(0)?;
            let cost: Option<f64> = row.get(1)?;
            Ok((tokens.unwrap_or(0), cost.unwrap_or(0.0)))
        };
        let (tokens, cost) = match parameter {
            Some(value) => self.connection.query_row(&sql, [value], read)?,
            None => self.connection.query_row(&sql, [], read)?,
        };
        Ok(UsageTotal {
            tokens: tokens.max(0) as u64,
            cost,
        })
    }

    /// The existing model table: rolling 24-hour totals, top six.
    fn models(&self, since_ms: i64) -> rusqlite::Result<Vec<ModelUsage>> {
        self.models_with_limit(since_ms, 6)
    }

    /// Model ranking over an arbitrary interval and row limit.
    ///
    /// Grouping is by `COALESCE(alias.model_id, event.model)` so one model seen
    /// through two clients (an omp `vendor/model` and an opencode `provider/model`)
    /// collapses into a single row instead of two.
    fn models_with_limit(&self, since_ms: i64, limit: i64) -> rusqlite::Result<Vec<ModelUsage>> {
        let mut statement = self.connection.prepare(
            "SELECT COALESCE(a.model_id, e.model)              AS model,
                    SUM(e.input_total)                         AS input_total,
                    SUM(e.cache_read)                          AS cache_read,
                    SUM(e.output_total)                        AS output,
                    SUM(e.reasoning)                           AS reasoning,
                    SUM(e.cost_usd)                            AS cost
             FROM usage_event e
             LEFT JOIN model_alias a ON a.raw_model = e.model
             WHERE e.occurred_at >= ?1
               AND e.model IS NOT NULL
               AND IFNULL(a.ignore, 0) = 0
             -- Grouping by the bare name `model` would bind to the base column
             -- `e.model`, not to the aliased expression, so one model reported
             -- under two client spellings would stay two rows.
             GROUP BY COALESCE(a.model_id, e.model)
             ORDER BY SUM(e.input_total + e.output_total) DESC, model ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(rusqlite::params![since_ms, limit], |row| {
            let input_total: Option<i64> = row.get(1)?;
            let cache_read: Option<i64> = row.get(2)?;
            let output: Option<i64> = row.get(3)?;
            let reasoning: Option<i64> = row.get(4)?;
            let cost: Option<f64> = row.get(5)?;
            Ok(ModelUsage {
                model: row.get(0)?,
                input_total: input_total.unwrap_or(0).max(0) as u64,
                cache_read: cache_read.unwrap_or(0).max(0) as u64,
                output: output.unwrap_or(0).max(0) as u64,
                reasoning: reasoning.unwrap_or(0).max(0) as u64,
                cost,
            })
        })?;
        rows.collect()
    }

    /// One histogram, bucketed by a local-time calendar field so a 28-day month
    /// cannot shift the bars and a quarter hour always lands in its own column.
    /// Tokens and money come back together, so the two charts over one period
    /// share a single grouped scan.
    fn buckets(&self, bucket: Bucket, since_ms: i64) -> rusqlite::Result<Bucketed> {
        let sql = format!(
            "SELECT {} AS bucket,
                    SUM(e.input_total + e.output_total),
                    SUM(e.cost_usd)
             FROM usage_event e
             WHERE e.occurred_at >= ?1
             GROUP BY bucket",
            bucket.expression()
        );
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map([since_ms], |row| {
            let index: i64 = row.get(0)?;
            let tokens: Option<i64> = row.get(1)?;
            let cost: Option<f64> = row.get(2)?;
            Ok((
                index,
                tokens.unwrap_or(0).max(0) as u64,
                cost.unwrap_or(0.0),
            ))
        })?;
        let mut buckets = vec![0u64; bucket.len()];
        let mut costs = vec![0f64; bucket.len()];
        for row in rows {
            let (index, tokens, cost) = row?;
            if let Some(slot) = buckets.get_mut(index.max(0) as usize) {
                *slot = tokens;
            }
            if let Some(slot) = costs.get_mut(index.max(0) as usize) {
                *slot = cost;
            }
        }
        Ok(Bucketed { buckets, costs })
    }
}

/// How a histogram cuts a local-time interval into buckets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bucket {
    /// Today in quarter hours: 96 slots, index 0 is 00:00.
    QuarterHour,
    /// This month in six-hour blocks: four slots per day, index 0 is the first
    /// day at 00:00 local.
    SixHour,
}

impl Bucket {
    /// The SQL expression that maps `occurred_at` (UTC ms) onto a local-time
    /// bucket index. Computed in SQL so a whole day or month is one grouped
    /// scan rather than one row fetched per event.
    fn expression(self) -> &'static str {
        match self {
            Self::QuarterHour => concat!(
                "CAST(strftime('%H', e.occurred_at / 1000, 'unixepoch', 'localtime') AS INTEGER) * 4",
                " + CAST(strftime('%M', e.occurred_at / 1000, 'unixepoch', 'localtime') AS INTEGER) / 15"
            ),
            Self::SixHour => concat!(
                "(CAST(strftime('%d', e.occurred_at / 1000, 'unixepoch', 'localtime') AS INTEGER) - 1) * 4",
                " + CAST(strftime('%H', e.occurred_at / 1000, 'unixepoch', 'localtime') AS INTEGER) / 6"
            ),
        }
    }

    /// Bucket count. The month follows the real month length, so February never
    /// draws a 31st.
    fn len(self) -> usize {
        match self {
            Self::QuarterHour => 96,
            Self::SixHour => {
                let now = Local::now();
                days_in_month(now.year(), now.month()) as usize * 4
            }
        }
    }
}

pub fn days_in_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let start = NaiveDate::from_ymd_opt(year, month, 1).expect("valid start of month");
    let next =
        NaiveDate::from_ymd_opt(next_year, next_month, 1).expect("valid start of next month");
    (next - start).num_days() as u32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Period {
    Day,
    Week,
    Month,
    Year,
}

/// Local-time start of the containing day / month / year, as UTC epoch ms.
///
/// `occurred_at` is stored in UTC, but "today" means today where the operator
/// sits, so the boundary is computed in the local zone and passed to SQL as an
/// instant.
fn local_midnight(now: chrono::DateTime<Local>, period: Period) -> i64 {
    let date = match period {
        Period::Day => now.date_naive(),
        Period::Week => {
            let days = now.weekday().num_days_from_monday();
            now.date_naive() - ChronoDuration::days(i64::from(days))
        }
        Period::Month => now.date_naive().with_day(1).unwrap_or(now.date_naive()),
        Period::Year => now
            .date_naive()
            .with_month(1)
            .and_then(|date| date.with_day(1))
            .unwrap_or(now.date_naive()),
    };
    let naive = date
        .and_hms_opt(0, 0, 0)
        .expect("midnight is always a valid time");
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|at| at.timestamp_millis())
        // A spring-forward day may not have a 00:00 instant; the next valid
        // moment is the honest boundary.
        .unwrap_or_else(|| {
            Local
                .from_local_datetime(&(naive + ChronoDuration::hours(1)))
                .earliest()
                .map(|at| at.timestamp_millis())
                .unwrap_or_else(|| now.timestamp_millis())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    fn seeded() -> Connection {
        let connection = Connection::open_in_memory().expect("memory db");
        connection
            .execute_batch(
                "CREATE TABLE usage_event(
                     source TEXT NOT NULL, event_id TEXT NOT NULL, model TEXT, model_source TEXT,
                     input_total INTEGER NOT NULL, cache_read INTEGER NOT NULL,
                     cache_write INTEGER NOT NULL, output_total INTEGER NOT NULL,
                     reasoning INTEGER NOT NULL, cost_usd REAL, occurred_at INTEGER NOT NULL,
                     PRIMARY KEY(source, event_id)) WITHOUT ROWID;
                 CREATE TABLE model_alias(
                     raw_model TEXT PRIMARY KEY, model_id TEXT, ignore INTEGER NOT NULL DEFAULT 0,
                     resolved_by TEXT NOT NULL, remark TEXT);",
            )
            .expect("schema");
        connection
    }

    #[allow(clippy::too_many_arguments)]
    fn insert(
        connection: &Connection,
        source: &str,
        id: &str,
        model: &str,
        input: i64,
        cache: i64,
        output: i64,
        cost: Option<f64>,
        at: i64,
    ) {
        connection
            .execute(
                "INSERT INTO usage_event VALUES (?1, ?2, ?3, 'event', ?4, ?5, 0, ?6, 0, ?7, ?8)",
                rusqlite::params![source, id, model, input, cache, output, cost, at],
            )
            .expect("insert");
    }

    fn reader(connection: Connection) -> Reader {
        Reader { connection }
    }

    #[test]
    fn totals_count_every_event_ignored_models_included() {
        let connection = seeded();
        // Two unpriced events of a model the ranking ignores, one priced event.
        insert(
            &connection,
            "opencode",
            "1",
            "opencode/free",
            10,
            0,
            1,
            None,
            100,
        );
        insert(
            &connection,
            "opencode",
            "2",
            "opencode/free",
            10,
            0,
            1,
            None,
            100,
        );
        insert(
            &connection,
            "omp",
            "3",
            "vendor/paid",
            10,
            0,
            1,
            Some(0.5),
            100,
        );
        connection
            .execute(
                "INSERT INTO model_alias VALUES ('opencode/free', NULL, 1, 'ignore', NULL)",
                [],
            )
            .expect("alias");
        let total = reader(connection).total(None).expect("total");
        // The alias is the ranking's filter, not the totals': every event counts
        // toward the interval figures, and unpriced events add no money.
        assert_eq!(total.tokens, 33);
        assert_eq!(total.cost, 0.5);
    }

    #[test]
    fn an_empty_table_totals_zero() {
        let total = reader(seeded()).total(None).expect("total");
        assert_eq!(total.tokens, 0);
        assert_eq!(total.cost, 0.0);
    }

    #[test]
    fn ignored_models_leave_the_model_table_entirely() {
        let connection = seeded();
        insert(
            &connection,
            "omp",
            "1",
            "vendor/paid",
            100,
            0,
            10,
            Some(1.0),
            100,
        );
        insert(
            &connection,
            "opencode",
            "2",
            "opencode/free",
            900,
            0,
            90,
            None,
            100,
        );
        connection
            .execute(
                "INSERT INTO model_alias VALUES ('opencode/free', NULL, 1, 'ignore', NULL)",
                [],
            )
            .expect("alias");
        let models = reader(connection).models(0).expect("models");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model, "vendor/paid");
    }

    #[test]
    fn one_model_seen_through_two_clients_is_a_single_row() {
        let connection = seeded();
        // The alias maps the client's name onto the price-table id, which is what
        // merges the two spellings.
        insert(
            &connection,
            "opencode",
            "1",
            "opencode-go/deepseek-v4-flash",
            100,
            0,
            10,
            Some(1.0),
            100,
        );
        insert(
            &connection,
            "omp",
            "2",
            "deepseek-v4-flash",
            200,
            0,
            20,
            Some(1.0),
            100,
        );
        connection
            .execute(
                "INSERT INTO model_alias VALUES ('opencode-go/deepseek-v4-flash', 'deepseek/deepseek-v4-flash', 0, 'bare', NULL)",
                [],
            )
            .expect("alias 1");
        connection
            .execute(
                "INSERT INTO model_alias VALUES ('deepseek-v4-flash', 'deepseek/deepseek-v4-flash', 0, 'bare', NULL)",
                [],
            )
            .expect("alias 2");
        let models = reader(connection).models(0).expect("models");
        assert_eq!(
            models.len(),
            1,
            "the same model must not split into two rows"
        );
        assert_eq!(models[0].model, "deepseek/deepseek-v4-flash");
        assert_eq!(models[0].total_tokens(), 330);
    }

    #[test]
    fn models_with_equal_totals_keep_a_stable_order() {
        let connection = seeded();
        insert(&connection, "omp", "1", "b", 100, 0, 0, Some(1.0), 100);
        insert(&connection, "omp", "2", "a", 100, 0, 0, Some(1.0), 100);
        let models = reader(connection).models(0).expect("models");
        let names: Vec<&str> = models.iter().map(|m| m.model.as_str()).collect();
        assert_eq!(names, vec!["a", "b"], "ties break on the model name");
    }

    #[test]
    fn monthly_model_query_can_return_ten() {
        let connection = seeded();
        for index in 0..12 {
            insert(
                &connection,
                "omp",
                &format!("id-{index}"),
                &format!("vendor/model-{index}"),
                100 + index,
                0,
                10,
                Some(1.0),
                100,
            );
        }
        let models = reader(connection)
            .models_with_limit(0, 10)
            .expect("monthly models");
        assert_eq!(models.len(), 10);
        assert_eq!(models[0].model, "vendor/model-11");
    }

    #[test]
    fn events_older_than_rolling_window_are_excluded_from_models() {
        let connection = seeded();
        // Event 1: occurred 25 hours ago (older than rolling 24H)
        insert(
            &connection,
            "omp",
            "1",
            "old-model",
            1000,
            0,
            100,
            Some(1.0),
            1000,
        );
        // Event 2: occurred 1 hour ago
        insert(
            &connection,
            "omp",
            "2",
            "recent-model",
            500,
            0,
            50,
            Some(0.5),
            100_000,
        );

        let models = reader(connection).models(50_000).expect("models");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model, "recent-model");
    }

    #[test]
    fn buckets_cover_the_whole_calendar_and_zero_fill_gaps() {
        let connection = seeded();
        let now = Local::now();
        let day_start = local_midnight(now, Period::Day);
        // Exactly one event, inside the current quarter hour, and priced.
        insert(
            &connection,
            "omp",
            "1",
            "m",
            10,
            0,
            1,
            Some(1.0),
            now.timestamp_millis(),
        );
        let buckets = reader(connection)
            .buckets(Bucket::QuarterHour, day_start)
            .expect("buckets");
        assert_eq!(buckets.buckets.len(), 96, "every quarter hour needs a slot");
        assert_eq!(buckets.costs.len(), 96, "money shares the token slots");
        let filled = buckets.buckets.iter().filter(|value| **value > 0).count();
        assert_eq!(
            filled, 1,
            "an absent quarter hour must stay a zero, not shift the row"
        );
        let slot = now.hour() as usize * 4 + now.minute() as usize / 15;
        assert_eq!(buckets.buckets[slot], 11);
        assert_eq!(
            buckets.costs[slot], 1.0,
            "the cost lands in the same bucket"
        );
    }

    #[test]
    fn the_month_histogram_follows_the_real_month_length() {
        let connection = seeded();
        let buckets = reader(connection)
            .buckets(Bucket::SixHour, 0)
            .expect("buckets");
        let days = days_in_month(Local::now().year(), Local::now().month()) as usize;
        assert_eq!(buckets.buckets.len(), days * 4, "four blocks per day");
        assert_eq!(buckets.costs.len(), days * 4);
        assert!(buckets.buckets.len() <= 31 * 4);
    }

    #[test]
    fn february_has_28_or_29_days_never_31() {
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2028, 2), 29);
        assert_eq!(days_in_month(2026, 4), 30);
        assert_eq!(days_in_month(2026, 12), 31);
    }

    #[test]
    fn days_in_month_pure_calendar() {
        // Leap years
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2025, 2), 28);
        // Year wrap-around: December -> January
        assert_eq!(days_in_month(2026, 12), 31);
        assert_eq!(days_in_month(2027, 1), 31);
        // All 12 months in a common year
        let expected = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        for (month, &days) in (1..=12).zip(&expected) {
            assert_eq!(days_in_month(2023, month), days);
        }
    }

    #[test]
    fn the_day_boundary_is_local_midnight_not_utc() {
        // `occurred_at` is UTC ms, but "today" is the operator's local day. At
        // UTC+8 the local midnight is 16:00 UTC on the previous date.
        let now = Local
            .with_ymd_and_hms(2026, 9, 11, 13, 30, 0)
            .single()
            .expect("valid instant");
        let start = local_midnight(now, Period::Day);
        let start_local = Local.timestamp_millis_opt(start).single().expect("valid");
        assert_eq!(start_local.hour(), 0);
        assert_eq!(start_local.day(), 11);
        assert_eq!(
            start_local.offset().local_minus_utc(),
            now.offset().local_minus_utc()
        );
    }

    #[test]
    fn the_month_and_year_boundaries_land_on_the_first() {
        let now = Local
            .with_ymd_and_hms(2026, 9, 11, 13, 30, 0)
            .single()
            .expect("valid instant");
        let month = Local
            .timestamp_millis_opt(local_midnight(now, Period::Month))
            .single()
            .expect("valid");
        assert_eq!((month.year(), month.month(), month.day()), (2026, 9, 1));
        let year = Local
            .timestamp_millis_opt(local_midnight(now, Period::Year))
            .single()
            .expect("valid");
        assert_eq!((year.year(), year.month(), year.day()), (2026, 1, 1));
    }

    #[test]
    fn the_week_boundary_lands_on_monday() {
        let now = Local
            .with_ymd_and_hms(2026, 4, 3, 15, 0, 0)
            .single()
            .expect("valid instant");
        let start = local_midnight(now, Period::Week);
        let week = Local.timestamp_millis_opt(start).single().expect("valid");
        assert_eq!((week.year(), week.month(), week.day()), (2026, 3, 30));
        assert_eq!(week.hour(), 0);
    }

    #[test]
    fn six_hour_buckets_split_a_day_into_four() {
        let connection = seeded();
        let now = Local::now();
        let day = NaiveDate::from_ymd_opt(now.year(), now.month(), 2).expect("day 2");
        let at = |hour: u32, minute: u32| {
            Local
                .from_local_datetime(&day.and_hms_opt(hour, minute, 0).expect("valid time"))
                .earliest()
                .expect("valid local time")
                .timestamp_millis()
        };
        // Day 2 at 00:30 and at 07:00: different six-hour blocks of the same day.
        insert(
            &connection,
            "omp",
            "a",
            "m",
            100,
            0,
            50,
            Some(1.5),
            at(0, 30),
        );
        insert(
            &connection,
            "omp",
            "b",
            "m",
            200,
            0,
            50,
            Some(2.5),
            at(7, 0),
        );
        let buckets = reader(connection)
            .buckets(Bucket::SixHour, local_midnight(now, Period::Month))
            .expect("buckets");
        assert_eq!(buckets.buckets[4], 150, "day 2, first block");
        assert_eq!(buckets.buckets[5], 250, "day 2, second block");
        assert_eq!(buckets.costs[4], 1.5, "money rides the same block");
        assert_eq!(buckets.costs[5], 2.5);
        assert!(
            buckets.buckets[..4].iter().all(|value| *value == 0),
            "day 1 must stay empty rather than shift the row"
        );
    }

    #[test]
    fn only_events_inside_the_interval_are_counted() {
        let connection = seeded();
        let now = Local::now();
        let day_start = local_midnight(now, Period::Day);
        insert(
            &connection,
            "omp",
            "inside",
            "m",
            10,
            0,
            1,
            Some(1.0),
            day_start + 1,
        );
        insert(
            &connection,
            "omp",
            "before",
            "m",
            999,
            0,
            99,
            Some(1.0),
            day_start - 1,
        );
        let total = reader(connection).total(Some(day_start)).expect("total");
        assert_eq!(total.tokens, 11, "the pre-midnight event is outside today");
    }
    #[test]
    fn production_db_rolling_24h_excludes_hy3() {
        let db_path = std::path::Path::new("/home/carter003/.local/share/herdr/usage.db");
        if !db_path.exists() {
            return;
        }
        let reader = Reader::open(db_path).expect("open production db");
        let stats = reader.load().expect("load stats");
        assert!(!stats.models.is_empty(), "models should not be empty");
        for m in &stats.models {
            assert!(
                !m.model.contains("hy3"),
                "hy3 should not be in 24H models: {}",
                m.model
            );
            assert!(
                !m.model.contains("ox-alpha-free"),
                "ox-alpha-free should not be in 24H models: {}",
                m.model
            );
            println!(
                "24H Model: {:<35} IN(HIT): {:<14} OUT: {:<8} COST: {:?}",
                m.model,
                m.input_display(),
                m.output,
                m.cost
            );
        }
    }
}
