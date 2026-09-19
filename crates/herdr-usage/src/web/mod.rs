//! Read-only local usage dashboard. Accounting matches input_total + output_total;
//! cache and reasoning are subsets and must never be added to that total again.
pub mod requests;
pub mod server;

use chrono::{Datelike, Duration, Local, NaiveDate, TimeZone};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

#[derive(Clone, Debug, Default, Serialize)]
pub struct Total {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub tokens: i64,
    pub cost_usd: Option<f64>,
    pub events: i64,
    pub unpriced: i64,
}
impl Total {
    fn add(&mut self, other: &Self) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.tokens += other.tokens;
        self.events += other.events;
        self.unpriced += other.unpriced;
        if let Some(cost) = other.cost_usd {
            *self.cost_usd.get_or_insert(0.0) += cost;
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct UsageRow {
    pub day: String,
    pub model: Option<String>,
    #[serde(flatten)]
    pub total: Total,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub today: String,
    pub timezone: String,
    pub start: String,
    pub end: String,
    pub first_day: Option<String>,
    pub models: Vec<Option<String>>,
    pub overview: BTreeMap<String, Total>,
    pub total: Total,
    pub daily: Vec<UsageRow>,
    pub details: Vec<UsageRow>,
    pub ranking: Vec<UsageRow>,
    pub overall_ranking: Vec<UsageRow>,
}

#[derive(Clone)]
struct GlobalStats {
    first_day: Option<String>,
    models: Vec<Option<String>>,
    overview: BTreeMap<String, Total>,
    overall_ranking: Vec<UsageRow>,
}

struct CachedGlobal {
    connection: Connection,
    data_version: i64,
    day: NaiveDate,
    stats: GlobalStats,
}

static GLOBAL_CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedGlobal>>> = OnceLock::new();

/// A bounded date range; model=None means all, Some(None) means unresolved.
#[derive(Default)]
pub struct Query {
    pub start: Option<NaiveDate>,
    pub end: Option<NaiveDate>,
    pub model: Option<Option<String>>,
}
impl Query {
    pub fn parse(query: &str) -> Result<Self, String> {
        let mut result = Self::default();
        for pair in query.split('&').filter(|s| !s.is_empty()) {
            let (key, value) = pair.split_once('=').ok_or("无效查询参数")?;
            let value = decode(value)?;
            match key {
                "start" | "end" => {
                    let date = NaiveDate::parse_from_str(&value, "%Y-%m-%d")
                        .map_err(|_| "日期格式应为 YYYY-MM-DD")?;
                    if date.to_string() != value {
                        return Err("日期格式应为 YYYY-MM-DD".into());
                    }
                    if key == "start" {
                        result.start = Some(date);
                    } else {
                        result.end = Some(date);
                    }
                }
                "model" => result.model = Some(Some(value)),
                "unknown" if value == "1" => result.model = Some(None),
                _ => return Err("未知查询参数".into()),
            }
        }
        Ok(result)
    }
}
fn decode(value: &str) -> Result<String, String> {
    let mut bytes = Vec::new();
    let mut iter = value.bytes();
    while let Some(byte) = iter.next() {
        bytes.push(match byte {
            b'+' => b' ',
            b'%' => {
                let high = iter
                    .next()
                    .and_then(|c| (c as char).to_digit(16))
                    .ok_or("无效编码")?;
                let low = iter
                    .next()
                    .and_then(|c| (c as char).to_digit(16))
                    .ok_or("无效编码")?;
                (high * 16 + low) as u8
            }
            byte => byte,
        });
    }
    String::from_utf8(bytes).map_err(|_| "无效 UTF-8".into())
}

pub fn load(path: &Path, query: Query) -> Result<Report, String> {
    let connection =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_| {
            "无法读取统计数据库，请确认 collector 已运行且 HERDR_USAGE_DB 路径正确".to_string()
        })?;
    connection
        .busy_timeout(std::time::Duration::from_secs(3))
        .map_err(|e| e.to_string())?;
    let today = Local::now().date_naive();
    let global = cached_global(path, today)?;
    report_with_global(&connection, query, today, global)
}

fn cached_global(path: &Path, today: NaiveDate) -> Result<GlobalStats, String> {
    let cache = GLOBAL_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(entry) = cache.get_mut(path) {
        // data_version changes on this persistent connection when another
        // connection commits inserts, price updates or alias edits.
        let version: i64 = entry
            .connection
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if version != entry.data_version || entry.day != today {
            entry.stats = global_stats(&entry.connection, today)?;
            entry.data_version = version;
            entry.day = today;
        }
        return Ok(entry.stats.clone());
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    connection
        .busy_timeout(std::time::Duration::from_secs(3))
        .map_err(|e| e.to_string())?;
    let version = connection
        .query_row("PRAGMA data_version", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let stats = global_stats(&connection, today)?;
    cache.insert(
        path.to_path_buf(),
        CachedGlobal {
            connection,
            data_version: version,
            day: today,
            stats: stats.clone(),
        },
    );
    Ok(stats)
}

#[cfg(test)]
fn report(connection: &Connection, query: Query, today: NaiveDate) -> Result<Report, String> {
    let global = global_stats(connection, today)?;
    report_with_global(connection, query, today, global)
}

fn usage_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<UsageRow> {
    let input: i64 = r.get(2)?;
    let output: i64 = r.get(3)?;
    Ok(UsageRow {
        day: r.get(0)?,
        model: r.get(1)?,
        total: Total {
            input,
            output,
            tokens: input + output,
            cache_read: r.get(4)?,
            cost_usd: r.get(5)?,
            events: r.get(6)?,
            unpriced: r.get(7)?,
        },
    })
}

fn global_stats(connection: &Connection, today: NaiveDate) -> Result<GlobalStats, String> {
    let mut statement = connection
        .prepare(
            "SELECT date(e.occurred_at / 1000, 'unixepoch', 'localtime'),
                COALESCE(a.model_id, e.model), SUM(e.input_total), SUM(e.output_total),
                SUM(e.cache_read), SUM(e.cost_usd), COUNT(*), SUM(e.cost_usd IS NULL)
         FROM usage_event e LEFT JOIN model_alias a ON a.raw_model = e.model
         GROUP BY 1, 2 ORDER BY 1, 2",
        )
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], usage_row)
        .map_err(|e| e.to_string())?;
    let mut overview: BTreeMap<String, Total> = ["today", "week", "month", "all"]
        .into_iter()
        .map(|key| (key.into(), Total::default()))
        .collect();
    let week = today - Duration::days(today.weekday().num_days_from_monday().into());
    let month = today.with_day(1).unwrap();
    let mut models = std::collections::BTreeSet::new();
    let mut overall_ranking = BTreeMap::<Option<String>, UsageRow>::new();
    let mut first_day = None;
    for row in rows {
        let row = row.map_err(|e| e.to_string())?;
        first_day.get_or_insert_with(|| row.day.clone());
        models.insert(row.model.clone());
        overall_ranking
            .entry(row.model.clone())
            .or_insert_with(|| UsageRow {
                day: String::new(),
                model: row.model.clone(),
                total: Total::default(),
            })
            .total
            .add(&row.total);
        let date = NaiveDate::parse_from_str(&row.day, "%Y-%m-%d").map_err(|e| e.to_string())?;
        for (key, since) in [
            ("today", today),
            ("week", week),
            ("month", month),
            ("all", NaiveDate::MIN),
        ] {
            if date >= since && (key == "all" || date <= today) {
                overview.get_mut(key).unwrap().add(&row.total);
            }
        }
    }
    let mut overall_ranking: Vec<_> = overall_ranking.into_values().collect();
    overall_ranking.sort_by(|a, b| {
        b.total
            .tokens
            .cmp(&a.total.tokens)
            .then(a.model.cmp(&b.model))
    });
    Ok(GlobalStats {
        first_day,
        models: models.into_iter().collect(),
        overview,
        overall_ranking,
    })
}

fn validated_bounds(query: &Query, today: NaiveDate) -> Result<(NaiveDate, NaiveDate), String> {
    let end = query.end.unwrap_or(today);
    let start = query.start.unwrap_or(end - Duration::days(29));
    if start > end {
        return Err("开始日期不能晚于结束日期".into());
    }
    if (end - start).num_days() > 3660 {
        return Err("单次查询最多支持 3661 天".into());
    }
    Ok((start, end))
}

fn report_with_global(
    connection: &Connection,
    query: Query,
    today: NaiveDate,
    global: GlobalStats,
) -> Result<Report, String> {
    let (start, end) = validated_bounds(&query, today)?;
    let from = Local
        .from_local_datetime(&start.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .ok_or("无法解析开始日期")?
        .timestamp_millis();
    let until = Local
        .from_local_datetime(
            &end.succ_opt()
                .ok_or("日期超出范围")?
                .and_hms_opt(0, 0, 0)
                .unwrap(),
        )
        .earliest()
        .ok_or("无法解析结束日期")?
        .timestamp_millis();
    let (model_mode, model_value) = match query.model.as_ref() {
        None => (0, None),
        Some(None) => (1, None),
        Some(Some(model)) => (2, Some(model.as_str())),
    };
    // The time predicate uses idx_usage_time before grouping. Alias filtering
    // happens in SQL, so a one-day query never groups the full history.
    let mut statement = connection
        .prepare(
            "SELECT date(e.occurred_at / 1000, 'unixepoch', 'localtime'),
                COALESCE(a.model_id, e.model), SUM(e.input_total), SUM(e.output_total),
                SUM(e.cache_read), SUM(e.cost_usd), COUNT(*), SUM(e.cost_usd IS NULL)
         FROM usage_event e LEFT JOIN model_alias a ON a.raw_model = e.model
         WHERE e.occurred_at >= ?1 AND e.occurred_at < ?2
           AND (?3 = 0 OR (?3 = 1 AND COALESCE(a.model_id, e.model) IS NULL)
                OR (?3 = 2 AND COALESCE(a.model_id, e.model) = ?4))
         GROUP BY 1, 2 ORDER BY 1, 2",
        )
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map(
            rusqlite::params![from, until, model_mode, model_value],
            usage_row,
        )
        .map_err(|e| e.to_string())?;
    let mut daily = BTreeMap::<String, UsageRow>::new();
    for offset in 0..=(end - start).num_days() {
        let day = (start + Duration::days(offset)).to_string();
        daily.insert(
            day.clone(),
            UsageRow {
                day,
                model: None,
                total: Total::default(),
            },
        );
    }
    let mut details = Vec::new();
    let mut ranking = BTreeMap::<Option<String>, UsageRow>::new();
    let mut total = Total::default();
    for row in rows {
        let row = row.map_err(|e| e.to_string())?;
        total.add(&row.total);
        daily.get_mut(&row.day).unwrap().total.add(&row.total);
        ranking
            .entry(row.model.clone())
            .or_insert_with(|| UsageRow {
                day: String::new(),
                model: row.model.clone(),
                total: Total::default(),
            })
            .total
            .add(&row.total);
        details.push(row);
    }
    let mut ranking: Vec<_> = ranking.into_values().collect();
    ranking.sort_by(|a, b| {
        b.total
            .tokens
            .cmp(&a.total.tokens)
            .then(a.model.cmp(&b.model))
    });
    Ok(Report {
        today: today.to_string(),
        timezone: Local::now().format("%Z (UTC %:z)").to_string(),
        start: start.to_string(),
        end: end.to_string(),
        first_day: global.first_day,
        models: global.models,
        overview: global.overview,
        total,
        daily: daily.into_values().collect(),
        details,
        ranking,
        overall_ranking: global.overall_ranking,
    })
}

#[cfg(test)]
mod tests;
