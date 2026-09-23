//! Hourly token consumption and monthly aggregation report.

use crate::db;
use chrono::{Datelike, Local, NaiveDate, Timelike};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Serialize;
use std::{collections::HashMap, path::Path};

#[derive(Debug, Clone, Serialize)]
pub struct HourPoint {
    pub hour: u8,
    pub input: i64,
    pub output: i64,
    pub tokens: i64,
    pub cache_read: i64,
    pub cost_usd: Option<f64>,
    pub events: i64,
    pub closed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DayHourlyRow {
    pub day: String,
    pub weekday: String,
    pub total_tokens: i64,
    pub total_cost: Option<f64>,
    pub hours: Vec<HourPoint>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MonthlySummary {
    pub total_tokens: i64,
    pub total_cost: Option<f64>,
    pub peak_day: String,
    pub peak_hour: u8,
    pub peak_tokens: i64,
    pub active_days: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct HourlyReport {
    pub month: String,
    pub available_months: Vec<String>,
    pub summary: MonthlySummary,
    pub days: Vec<DayHourlyRow>,
}

fn weekday_zh(date: NaiveDate) -> &'static str {
    match date.weekday() {
        chrono::Weekday::Mon => "周一",
        chrono::Weekday::Tue => "周二",
        chrono::Weekday::Wed => "周三",
        chrono::Weekday::Thu => "周四",
        chrono::Weekday::Fri => "周五",
        chrono::Weekday::Sat => "周六",
        chrono::Weekday::Sun => "周日",
    }
}

pub fn load(path: &Path, month_param: Option<&str>) -> Result<HourlyReport, String> {
    let connection = match db::open(path) {
        Ok(mut conn) => {
            let now_ms = chrono::Utc::now().timestamp_millis();
            let _ = db::rollup_closed_hours(&mut conn, now_ms);
            conn
        }
        Err(_) => Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|_| "无法读取统计数据库，请确认 collector 已运行".to_string())?,
    };
    let _ = connection.busy_timeout(std::time::Duration::from_secs(3));

    let mut available_months: Vec<String> = {
        let mut stmt = connection
            .prepare(
                "SELECT DISTINCT substr(day, 1, 7) FROM usage_hourly
                 UNION
                 SELECT DISTINCT strftime('%Y-%m', occurred_at / 1000, 'unixepoch', 'localtime') FROM usage_event
                 ORDER BY 1 DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.filter_map(Result::ok).collect()
    };

    let current_month_str = Local::now().format("%Y-%m").to_string();
    if !available_months.contains(&current_month_str) {
        available_months.push(current_month_str.clone());
    }
    available_months.sort_by(|a, b| b.cmp(a));

    let month = month_param.unwrap_or(&current_month_str);
    let start_of_month = NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d")
        .map_err(|_| format!("无效的月份格式: {month}，应为 YYYY-MM"))?;
    if format!("{:04}-{:02}", start_of_month.year(), start_of_month.month()) != month {
        return Err(format!("无效的月份格式: {month}，应为 YYYY-MM"));
    }
    let today = Local::now().date_naive();
    let current_hour = Local::now().hour() as u8;
    let next_month = if start_of_month.month() == 12 {
        NaiveDate::from_ymd_opt(start_of_month.year() + 1, 1, 1).unwrap()
    } else {
        NaiveDate::from_ymd_opt(start_of_month.year(), start_of_month.month() + 1, 1).unwrap()
    };
    let last_day_of_month = next_month.pred_opt().unwrap();

    let latest_day = if start_of_month.year() == today.year() && start_of_month.month() == today.month() {
        today
    } else if start_of_month > today {
        start_of_month
    } else {
        last_day_of_month
    };

    let mut hourly_map: HashMap<(String, u8), (i64, i64, i64, i64, Option<f64>, i64)> =
        HashMap::new();
    {
        let mut stmt = connection
            .prepare(
                "SELECT day, hour, input_total, output_total, tokens, cache_read, cost_usd, events
                 FROM usage_hourly
                 WHERE day >= ?1 AND day <= ?2 AND model = ''",
            )
            .map_err(|e| e.to_string())?;
        let start_str = start_of_month.to_string();
        let end_str = last_day_of_month.to_string();
        let rows = stmt
            .query_map(rusqlite::params![start_str, end_str], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u8>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<f64>>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (day, hour, inp, out, tok, cache, cost, evs) = row.map_err(|e| e.to_string())?;
            hourly_map.insert((day, hour), (inp, out, tok, cache, cost, evs));
        }
    }

    if start_of_month.year() == today.year() && start_of_month.month() == today.month() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        if let Some(hour_start_ms) = db::compute_current_hour_start_ms(now_ms) {
            let mut stmt = connection
                .prepare(
                    "SELECT SUM(input_total), SUM(output_total), SUM(input_total + output_total),
                            SUM(cache_read), SUM(cost_usd), COUNT(*)
                     FROM usage_event
                     WHERE occurred_at >= ?1",
                )
                .map_err(|e| e.to_string())?;
            let in_progress = stmt
                .query_row([hour_start_ms], |row| {
                    Ok((
                        row.get::<_, Option<i64>>(0)?.unwrap_or(0),
                        row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                        row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                        row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                        row.get::<_, Option<f64>>(4)?,
                        row.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    ))
                })
                .optional()
                .map_err(|e| e.to_string())?;
            if let Some((inp, out, tok, cache, cost, evs)) = in_progress {
                if evs > 0 {
                    hourly_map.insert(
                        (today.to_string(), current_hour),
                        (inp, out, tok, cache, cost, evs),
                    );
                }
            }
        }
    }

    let mut days = Vec::new();
    let mut cur = latest_day;
    while cur >= start_of_month {
        let day_str = cur.to_string();
        let weekday = weekday_zh(cur).to_string();
        let mut hours = Vec::with_capacity(24);
        for h in 0..24 {
            let closed = if cur < today {
                true
            } else if cur == today {
                h < current_hour
            } else {
                false
            };
            let (input, output, tokens, cache_read, cost_usd, events) = hourly_map
                .get(&(day_str.clone(), h))
                .cloned()
                .unwrap_or((0, 0, 0, 0, None, 0));
            hours.push(HourPoint {
                hour: h,
                input,
                output,
                tokens,
                cache_read,
                cost_usd,
                events,
                closed,
            });
        }
        let total_tokens: i64 = hours.iter().map(|h| h.tokens).sum();
        let mut cost_sum = 0.0;
        let mut has_cost = false;
        for h in &hours {
            if let Some(c) = h.cost_usd {
                cost_sum += c;
                has_cost = true;
            }
        }
        let total_cost = if has_cost { Some(cost_sum) } else { None };
        days.push(DayHourlyRow {
            day: day_str,
            weekday,
            total_tokens,
            total_cost,
            hours,
        });
        if cur == start_of_month {
            break;
        }
        cur = cur.pred_opt().unwrap();
    }

    let total_tokens: i64 = days.iter().map(|d| d.total_tokens).sum();
    let mut month_cost_sum = 0.0;
    let mut month_has_cost = false;
    for d in &days {
        if let Some(c) = d.total_cost {
            month_cost_sum += c;
            month_has_cost = true;
        }
    }
    let total_cost = if month_has_cost {
        Some(month_cost_sum)
    } else {
        None
    };

    let mut peak_day = String::new();
    let mut peak_hour = 0u8;
    let mut peak_tokens = 0i64;
    for d in &days {
        for h in &d.hours {
            if h.tokens > peak_tokens {
                peak_tokens = h.tokens;
                peak_day = d.day.clone();
                peak_hour = h.hour;
            }
        }
    }

    let active_days = days.iter().filter(|d| d.total_tokens > 0).count();

    let summary = MonthlySummary {
        total_tokens,
        total_cost,
        peak_day,
        peak_hour,
        peak_tokens,
        active_days,
    };

    Ok(HourlyReport {
        month: month.to_string(),
        available_months,
        summary,
        days,
    })
}
