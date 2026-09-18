//! Read-only local usage dashboard. Accounting matches input_total + output_total;
//! cache and reasoning are subsets and must never be added to that total again.
pub mod requests;
pub mod server;

use chrono::{Datelike, Duration, Local, NaiveDate};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::{collections::BTreeMap, path::Path};

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
    report(&connection, query, Local::now().date_naive())
}
fn report(connection: &Connection, query: Query, today: NaiveDate) -> Result<Report, String> {
    let end = query.end.unwrap_or(today);
    let start = query.start.unwrap_or(end - Duration::days(29));
    if start > end {
        return Err("开始日期不能晚于结束日期".into());
    }
    if (end - start).num_days() > 3660 {
        return Err("单次查询最多支持 3661 天".into());
    }
    // One grouped snapshot keeps overview, model totals and detail reconciled.
    // Retain ignored and unresolved models: their tokens are part of the totals.
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
        .query_map([], |r| {
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
        })
        .map_err(|e| e.to_string())?;
    let mut overview: BTreeMap<String, Total> = ["today", "week", "month", "all"]
        .into_iter()
        .map(|key| (key.into(), Total::default()))
        .collect();
    let week = today - Duration::days(today.weekday().num_days_from_monday().into());
    let month = today.with_day(1).unwrap();
    let mut models = std::collections::BTreeSet::new();
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
    let mut overall_ranking = BTreeMap::<Option<String>, UsageRow>::new();
    let mut total = Total::default();
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
        if date < start || date > end || query.model.as_ref().is_some_and(|m| m != &row.model) {
            continue;
        }
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
    let mut overall_ranking: Vec<_> = overall_ranking.into_values().collect();
    overall_ranking.sort_by(|a, b| {
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
        first_day,
        models: models.into_iter().collect(),
        overview,
        total,
        daily: daily.into_values().collect(),
        details,
        ranking,
        overall_ranking,
    })
}

#[cfg(test)]
mod tests;
