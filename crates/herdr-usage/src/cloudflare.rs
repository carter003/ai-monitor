//! Cloudflare Workers, D1, R2, KV, and Observability usage statistics engine.

use chrono::{Datelike, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CloudflareConfig {
    pub account_id: Option<String>,
    pub api_token: Option<String>,
    pub plan: String, // "paid" | "free"
    pub billing_day: Option<u8>, // 1..=31, e.g. 22
}

impl CloudflareConfig {
    pub fn is_configured(&self) -> bool {
        self.account_id
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
            && self
                .api_token
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
    }

    pub fn from_env_or_config(
        account_id: Option<String>,
        api_token: Option<String>,
        plan: Option<String>,
        billing_day: Option<u8>,
    ) -> Self {
        let account_id = std::env::var("CLOUDFLARE_ACCOUNT_ID")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or(account_id.filter(|s| !s.trim().is_empty()));
        let api_token = std::env::var("CLOUDFLARE_API_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or(api_token.filter(|s| !s.trim().is_empty()));
        let plan_val = std::env::var("CLOUDFLARE_PLAN")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or(plan.filter(|s| !s.trim().is_empty()))
            .unwrap_or_else(|| "paid".to_string());
        let plan = if plan_val.eq_ignore_ascii_case("free") {
            "free".to_string()
        } else {
            "paid".to_string()
        };
        let billing_day = std::env::var("CLOUDFLARE_BILLING_DAY")
            .ok()
            .and_then(|s| s.parse::<u8>().ok())
            .filter(|d| (1..=31).contains(d))
            .or(billing_day.filter(|d| (1..=31).contains(d)));

        Self {
            account_id,
            api_token,
            plan,
            billing_day,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CloudflareQuotas {
    pub workers_requests: f64,
    pub workers_cpu_time_us: f64,
    pub d1_rows_read: f64,
    pub d1_rows_written: f64,
    pub d1_storage_bytes: f64,
    pub r2_class_a: f64,
    pub r2_class_b: f64,
    pub r2_operations: f64,
    pub r2_storage_bytes: f64,
    pub kv_read_operations: f64,
    pub kv_write_operations: f64,
    pub kv_storage_bytes: f64,
    pub analytics_engine_points: f64,
    pub observability_events: f64,
}

impl CloudflareQuotas {
    pub fn for_plan(plan: &str) -> Self {
        if plan.eq_ignore_ascii_case("free") {
            Self {
                workers_requests: 3_000_000.0,         // ~100k/day * 30
                workers_cpu_time_us: 30_000_000_000.0, // 30,000s
                d1_rows_read: 150_000_000.0,           // 5M/day * 30
                d1_rows_written: 3_000_000.0,          // 100k/day * 30
                d1_storage_bytes: 500_000_000.0,       // 500 MB
                r2_class_a: 1_000_000.0,               // 1M Class A
                r2_class_b: 10_000_000.0,              // 10M Class B
                r2_operations: 1_000_000.0,
                r2_storage_bytes: 10_000_000_000.0,    // 10 GB
                kv_read_operations: 3_000_000.0,       // 100k/day * 30
                kv_write_operations: 30_000.0,         // 1k/day * 30
                kv_storage_bytes: 1_000_000_000.0,     // 1 GB
                analytics_engine_points: 1_000_000.0,
                observability_events: 6_000_000.0,     // 200k/day * 30
            }
        } else {
            Self {
                workers_requests: 10_000_000.0,        // 10M included
                workers_cpu_time_us: 30_000_000_000.0, // 30M ms = 30,000s included ($0.02 / 1M ms thereafter)
                d1_rows_read: 25_000_000_000.0,        // 25B included
                d1_rows_written: 50_000_000.0,         // 50M included
                d1_storage_bytes: 5_000_000_000.0,     // 5 GB included ($0.75/GB-mo thereafter)
                r2_class_a: 1_000_000.0,               // 1M Class A included
                r2_class_b: 10_000_000.0,              // 10M Class B included
                r2_operations: 1_000_000.0,            // 1M Class A included
                r2_storage_bytes: 10_000_000_000.0,    // 10 GB included ($0.015/GB-mo thereafter)
                kv_read_operations: 10_000_000.0,      // 10M included ($0.50/M thereafter)
                kv_write_operations: 1_000_000.0,      // 1M included ($5.00/M thereafter)
                kv_storage_bytes: 1_000_000_000.0,      // 1 GB included ($0.50/GB-mo thereafter)
                analytics_engine_points: 10_000_000.0, // 10M included ($0.25/M thereafter)
                observability_events: 20_000_000.0,    // 20M events included ($0.60/M thereafter)
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BillingCycleInfo {
    pub start: String,
    pub end: String,
    pub start_date: String,
    pub end_date: String,
    pub total_days: i64,
    pub elapsed_days: i64,
    pub days_remaining: i64,
    pub elapsed_percent: f64,
    pub cycle_type: String,
    pub is_fallback: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetricItem {
    pub name: String,
    pub value: f64,
    pub quota: f64,
    pub remaining: f64,
    pub percent: f64,
    pub formatted_value: String,
    pub formatted_quota: String,
    pub formatted_remaining: String,
    pub unit: String,
    pub status: String, // "normal" | "warning" | "danger"
    pub progress_bar: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CurrentPeriodSummary {
    // Workers Compute
    pub workers_requests: MetricItem,
    pub workers_cpu_time: MetricItem,
    pub workers_errors: MetricItem,

    // D1
    pub d1_rows_read: MetricItem,
    pub d1_rows_written: MetricItem,
    pub d1_storage: MetricItem,
    pub d1_read_queries: u64,
    pub d1_write_queries: u64,
    pub d1_databases_count: u64,

    // R2
    pub r2_class_a_operations: MetricItem,
    pub r2_class_b_operations: MetricItem,
    pub r2_operations: MetricItem,
    pub r2_storage: MetricItem,
    pub r2_buckets_count: u64,
    pub r2_objects_count: u64,

    // Workers KV
    pub kv_read_operations: MetricItem,
    pub kv_write_operations: MetricItem,
    pub kv_storage: MetricItem,
    pub kv_namespaces_count: u64,

    // Observability & Limits
    pub observability_events: MetricItem,
    pub analytics_engine_points: MetricItem,
    pub subrequests_limit: u64,
    pub live_tail_limit: u64,
    pub logpush_jobs_limit: u64,
    pub cron_triggers_limit: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DailyUsage {
    pub date: String,
    pub workers_requests: u64,
    pub workers_cpu_time_us: u64,
    pub workers_errors: u64,
    pub d1_rows_read: u64,
    pub d1_rows_written: u64,
    pub r2_operations: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MonthlyUsage {
    pub month: String,
    pub workers_requests: u64,
    pub workers_cpu_time_us: u64,
    pub workers_errors: u64,
    pub d1_rows_read: u64,
    pub d1_rows_written: u64,
    pub r2_operations: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CloudflareReport {
    pub configured: bool,
    pub plan: String,
    pub billing_cycle: BillingCycleInfo,
    pub current_period: CurrentPeriodSummary,
    pub ascii_card: String,
    pub daily_trend: Vec<DailyUsage>,
    pub monthly_history: Vec<MonthlyUsage>,
    pub stale: bool,
    pub error: Option<String>,
    pub synced_at: i64,
}

// ---------------------------------------------------------------------------
// Formatting Helpers
// ---------------------------------------------------------------------------

pub fn format_with_commas(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    let offset = s.len() % 3;
    if offset > 0 {
        result.push_str(&s[..offset]);
    }
    for (i, ch) in s[offset..].chars().enumerate() {
        if i % 3 == 0 && (offset > 0 || i > 0) {
            result.push(',');
        }
        result.push(ch);
    }
    result
}

pub fn format_compact_number(n: f64) -> String {
    if n >= 1_000_000_000.0 {
        let val = n / 1_000_000_000.0;
        format_one_or_two_decimals(val, "B")
    } else if n >= 1_000_000.0 {
        let val = n / 1_000_000.0;
        format_one_or_two_decimals(val, "M")
    } else if n >= 10_000.0 {
        let val = n / 1_000.0;
        if val >= 100.0 || (val * 10.0).fract() == 0.0 {
            format!("{:.0}K", val)
        } else {
            format!("{:.1}K", val)
        }
    } else {
        format_with_commas(n.round() as u64)
    }
}

fn format_one_or_two_decimals(val: f64, unit: &str) -> String {
    let rounded_one = (val * 10.0).round() / 10.0;
    if (val - rounded_one).abs() < 0.01 {
        format!("{:.1}{}", rounded_one, unit)
    } else {
        format!("{:.2}{}", val, unit)
    }
}

pub fn format_cpu_time(cpu_time_us: f64) -> String {
    let sec = cpu_time_us / 1_000_000.0;
    if sec >= 10_000.0 {
        format!("{:.0} sec", sec)
    } else if sec >= 1.0 {
        format!("{:.1} sec", sec)
    } else {
        let ms = cpu_time_us / 1_000.0;
        format!("{:.1} ms", ms)
    }
}

pub fn format_bytes(bytes: f64) -> String {
    if bytes >= 1_000_000_000.0 {
        format!("{:.2} GB", bytes / 1_000_000_000.0)
    } else if bytes >= 1_000_000.0 {
        format!("{:.1} MB", bytes / 1_000_000.0)
    } else if bytes >= 1_000.0 {
        format!("{:.1} KB", bytes / 1_000.0)
    } else {
        format!("{:.0} B", bytes)
    }
}

pub fn format_percent(pct: f64) -> String {
    if pct < 0.1 && pct > 0.0 {
        format!("{:.2}%", pct)
    } else {
        format!("{:.1}%", pct)
    }
}

pub fn format_progress_bar(pct: f64, width: usize) -> String {
    let ratio = (pct / 100.0).clamp(0.0, 1.0);
    let filled = ((ratio * width as f64).round() as usize).min(width);
    let empty = width.saturating_sub(filled);
    format!("{}{}", "█".repeat(filled), " ".repeat(empty))
}

pub fn generate_ascii_card(summary: &CurrentPeriodSummary) -> String {
    let format_row = |label: &str, val_str: &str, pct_str: &str, bar: &str| -> String {
        let bar_9 = if bar.chars().count() > 9 {
            bar.chars().take(9).collect::<String>()
        } else {
            format!("{:<9}", bar)
        };
        format!(
            "│ {:<15}{:<13}{:>6} {:<9}│",
            label, val_str, pct_str, bar_9
        )
    };

    let mut lines = Vec::new();
    lines.push(format!("┌{}┐", "─".repeat(45)));
    lines.push(format!("│ {:<44}│", "WORKERS"));
    lines.push(format_row(
        "Requests",
        &summary.workers_requests.formatted_value,
        &format_percent(summary.workers_requests.percent),
        &summary.workers_requests.progress_bar,
    ));
    lines.push(format_row(
        "CPU Time",
        &summary.workers_cpu_time.formatted_value,
        &format_percent(summary.workers_cpu_time.percent),
        &summary.workers_cpu_time.progress_bar,
    ));
    lines.push(format_row(
        "Errors",
        &summary.workers_errors.formatted_value,
        &format_percent(summary.workers_errors.percent),
        "",
    ));
    lines.push(format!("│ {:<44}│", ""));
    lines.push(format!("│ {:<44}│", "D1"));
    lines.push(format_row(
        "Rows Read",
        &summary.d1_rows_read.formatted_value,
        &format_percent(summary.d1_rows_read.percent),
        &summary.d1_rows_read.progress_bar,
    ));
    lines.push(format_row(
        "Rows Written",
        &summary.d1_rows_written.formatted_value,
        &format_percent(summary.d1_rows_written.percent),
        &summary.d1_rows_written.progress_bar,
    ));
    lines.push(format!("│ {:<44}│", ""));
    lines.push(format!("│ {:<44}│", "R2"));
    lines.push(format_row(
        "Operations",
        &summary.r2_operations.formatted_value,
        &format_percent(summary.r2_operations.percent),
        &summary.r2_operations.progress_bar,
    ));
    lines.push(format!("└{}┘", "─".repeat(45)));

    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Billing Period Calculation
// ---------------------------------------------------------------------------

pub fn compute_cycle_info(
    start: String,
    end: String,
    start_date: String,
    end_date: String,
    cycle_type: String,
    is_fallback: bool,
    now: chrono::DateTime<Utc>,
) -> BillingCycleInfo {
    let start_dt = chrono::DateTime::parse_from_rfc3339(&start).ok();
    let end_dt = chrono::DateTime::parse_from_rfc3339(&end).ok();

    let (total_days, elapsed_days, days_remaining, elapsed_percent) = match (start_dt, end_dt) {
        (Some(s), Some(e)) => {
            let total = (e - s).num_days().max(1);
            let elapsed = (now.fixed_offset() - s).num_days().max(0);
            let rem = (e - now.fixed_offset()).num_days().max(0);
            let pct = (elapsed as f64 / total as f64 * 100.0).clamp(0.0, 100.0);
            (total, elapsed, rem, pct)
        }
        _ => (30, 0, 30, 0.0),
    };

    BillingCycleInfo {
        start,
        end,
        start_date,
        end_date,
        total_days,
        elapsed_days,
        days_remaining,
        elapsed_percent,
        cycle_type,
        is_fallback,
    }
}

pub fn calculate_billing_period_with_day(
    now: chrono::DateTime<Utc>,
    billing_day: u8,
) -> BillingCycleInfo {
    let year = now.year();
    let month = now.month();
    let day = now.day() as u8;

    let (start_year, start_month, end_year, end_month) = if day >= billing_day {
        let (ny, nm) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
        (year, month, ny, nm)
    } else {
        let (py, pm) = if month == 1 { (year - 1, 12) } else { (year, month - 1) };
        (py, pm, year, month)
    };

    let start_date_str = format!("{start_year:04}-{start_month:02}-{billing_day:02}");
    let end_date_str = format!("{end_year:04}-{end_month:02}-{billing_day:02}");

    let start = format!("{start_date_str}T00:00:00Z");
    let end = format!("{end_date_str}T00:00:00Z");

    compute_cycle_info(
        start,
        end,
        start_date_str,
        end_date_str,
        format!("套餐订阅周期 (每月 {billing_day} 日重置)"),
        false,
        now,
    )
}

pub fn fallback_billing_period(now: chrono::DateTime<Utc>) -> BillingCycleInfo {
    let year = now.year();
    let month = now.month();
    let start = format!("{year:04}-{month:02}-01T00:00:00Z");
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let end = format!("{next_year:04}-{next_month:02}-01T00:00:00Z");
    let start_date = format!("{year:04}-{month:02}-01");
    let end_date = format!("{next_year:04}-{next_month:02}-01");

    compute_cycle_info(
        start,
        end,
        start_date,
        end_date,
        "自然月周期 (可通过配置指定套餐重置日)".into(),
        true,
        now,
    )
}

#[derive(Deserialize)]
struct SubscriptionsResponse {
    result: Option<Vec<SubscriptionItem>>,
}

#[derive(Deserialize)]
struct SubscriptionItem {
    current_period_start: Option<String>,
    current_period_end: Option<String>,
}

pub fn fetch_billing_period(
    client: &reqwest::blocking::Client,
    account_id: &str,
    api_token: &str,
    billing_day: Option<u8>,
    now: chrono::DateTime<Utc>,
) -> BillingCycleInfo {
    let url = format!("https://api.cloudflare.com/client/v4/accounts/{account_id}/subscriptions");
    let response = client
        .get(&url)
        .header("Authorization", format!("Bearer {api_token}"))
        .send();

    if let Ok(resp) = response {
        if resp.status().is_success() {
            if let Ok(body) = resp.json::<SubscriptionsResponse>() {
                if let Some(items) = body.result {
                    for item in items {
                        if let (Some(start), Some(end)) =
                            (item.current_period_start, item.current_period_end)
                        {
                            let start_date = start.get(..10).unwrap_or(&start).to_string();
                            let end_date = end.get(..10).unwrap_or(&end).to_string();
                            return compute_cycle_info(
                                start,
                                end,
                                start_date,
                                end_date,
                                "套餐订阅周期 (API 自动获取)".into(),
                                false,
                                now,
                            );
                        }
                    }
                }
            }
        }
    }

    if let Some(day) = billing_day {
        return calculate_billing_period_with_day(now, day);
    }

    fallback_billing_period(now)
}

// ---------------------------------------------------------------------------
// Extended Storage Details via REST API
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct StorageDetails {
    pub d1_total_bytes: u64,
    pub d1_databases_count: u64,
    pub r2_total_bytes: u64,
    pub r2_buckets_count: u64,
    pub r2_objects_count: u64,
    pub kv_namespaces_count: u64,
}

pub fn fetch_storage_details(
    client: &reqwest::blocking::Client,
    account_id: &str,
    api_token: &str,
) -> StorageDetails {
    let mut details = StorageDetails::default();

    // 1. D1 databases & sizes
    let d1_url = format!("https://api.cloudflare.com/client/v4/accounts/{account_id}/d1/database");
    if let Ok(resp) = client.get(&d1_url).header("Authorization", format!("Bearer {api_token}")).send() {
        if let Ok(json) = resp.json::<serde_json::Value>() {
            if let Some(arr) = json.get("result").and_then(|r| r.as_array()) {
                details.d1_databases_count = arr.len() as u64;
                for db in arr {
                    details.d1_total_bytes += db.get("file_size").and_then(|s| s.as_u64()).unwrap_or(0);
                }
            }
        }
    }

    // 2. R2 buckets & usage
    let r2_url = format!("https://api.cloudflare.com/client/v4/accounts/{account_id}/r2/buckets");
    if let Ok(resp) = client.get(&r2_url).header("Authorization", format!("Bearer {api_token}")).send() {
        if let Ok(json) = resp.json::<serde_json::Value>() {
            if let Some(arr) = json.get("result").and_then(|r| r.get("buckets")).and_then(|b| b.as_array()) {
                details.r2_buckets_count = arr.len() as u64;
                for bucket in arr {
                    if let Some(name) = bucket.get("name").and_then(|n| n.as_str()) {
                        let usage_url = format!("https://api.cloudflare.com/client/v4/accounts/{account_id}/r2/buckets/{name}/usage");
                        if let Ok(u_resp) = client.get(&usage_url).header("Authorization", format!("Bearer {api_token}")).send() {
                            if let Ok(u_json) = u_resp.json::<serde_json::Value>() {
                                if let Some(res) = u_json.get("result") {
                                    let p_size = res.get("payloadSize")
                                        .and_then(|s| s.as_str())
                                        .and_then(|s| s.parse::<u64>().ok())
                                        .unwrap_or(0);
                                    let o_count = res.get("objectCount")
                                        .and_then(|s| s.as_str())
                                        .and_then(|s| s.parse::<u64>().ok())
                                        .unwrap_or(0);
                                    details.r2_total_bytes += p_size;
                                    details.r2_objects_count += o_count;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // 3. KV namespaces
    let kv_url = format!("https://api.cloudflare.com/client/v4/accounts/{account_id}/storage/kv/namespaces");
    if let Ok(resp) = client.get(&kv_url).header("Authorization", format!("Bearer {api_token}")).send() {
        if let Ok(json) = resp.json::<serde_json::Value>() {
            if let Some(arr) = json.get("result").and_then(|r| r.as_array()) {
                details.kv_namespaces_count = arr.len() as u64;
            }
        }
    }

    details
}

// ---------------------------------------------------------------------------
// GraphQL Analytics API Query & Parser
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct GqlResponse {
    data: Option<GqlViewer>,
    errors: Option<Vec<GqlError>>,
}

#[derive(Deserialize, Default)]
struct GqlError {
    message: String,
}

#[derive(Deserialize, Default)]
struct GqlViewer {
    viewer: Option<GqlAccounts>,
}

#[derive(Deserialize, Default)]
struct GqlAccounts {
    accounts: Option<Vec<GqlAccountNode>>,
}

#[derive(Deserialize, Default)]
struct GqlAccountNode {
    #[serde(default, rename = "workersOverview")]
    workers_overview: Option<Vec<GqlWorkersOverviewItem>>,
    #[serde(default, rename = "workersDaily")]
    workers_daily: Option<Vec<GqlWorkersDailyItem>>,
    #[serde(default, rename = "d1Overview")]
    d1_overview: Option<Vec<GqlD1OverviewItem>>,
    #[serde(default, rename = "d1Daily")]
    d1_daily: Option<Vec<GqlD1DailyItem>>,
    #[serde(default, rename = "r2Overview")]
    r2_overview: Option<Vec<GqlR2OverviewItem>>,
    #[serde(default, rename = "r2Daily")]
    r2_daily: Option<Vec<GqlR2DailyItem>>,
    #[serde(default, rename = "kvOperations")]
    kv_operations: Option<Vec<GqlKvOperationItem>>,
    #[serde(default, rename = "analyticsEngine")]
    analytics_engine: Option<Vec<GqlAnalyticsEngineItem>>,
}

#[derive(Deserialize, Default)]
struct GqlWorkersOverviewItem {
    sum: Option<GqlWorkersSum>,
}

#[derive(Deserialize, Default)]
struct GqlWorkersDailyItem {
    dimensions: Option<GqlDateDimension>,
    sum: Option<GqlWorkersSum>,
}

#[derive(Deserialize, Default)]
struct GqlWorkersSum {
    requests: Option<u64>,
    errors: Option<u64>,
    #[serde(rename = "cpuTimeUs")]
    cpu_time_us: Option<u64>,
}

#[derive(Deserialize, Default)]
struct GqlD1OverviewItem {
    sum: Option<GqlD1Sum>,
}

#[derive(Deserialize, Default)]
struct GqlD1DailyItem {
    dimensions: Option<GqlDateDimension>,
    sum: Option<GqlD1Sum>,
}

#[derive(Deserialize, Default)]
#[allow(dead_code)]
struct GqlD1Sum {
    #[serde(rename = "rowsRead")]
    rows_read: Option<u64>,
    #[serde(rename = "rowsWritten")]
    rows_written: Option<u64>,
    #[serde(rename = "readQueries")]
    read_queries: Option<u64>,
    #[serde(rename = "writeQueries")]
    write_queries: Option<u64>,
}

#[derive(Deserialize, Default)]
#[allow(dead_code)]
struct GqlR2OverviewItem {
    dimensions: Option<GqlR2ActionDimension>,
    sum: Option<GqlR2Sum>,
}

#[derive(Deserialize, Default)]
struct GqlR2DailyItem {
    dimensions: Option<GqlDateDimension>,
    sum: Option<GqlR2Sum>,
}

#[derive(Deserialize, Default)]
#[allow(dead_code)]
struct GqlR2ActionDimension {
    #[serde(rename = "actionType")]
    action_type: Option<String>,
}

#[derive(Deserialize, Default)]
struct GqlDateDimension {
    date: Option<String>,
}

#[derive(Deserialize, Default)]
#[allow(dead_code)]
struct GqlR2Sum {
    requests: Option<u64>,
    #[serde(rename = "responseObjectSize")]
    response_object_size: Option<u64>,
}

#[derive(Deserialize, Default)]
struct GqlKvOperationItem {
    dimensions: Option<GqlKvActionDimension>,
    sum: Option<GqlKvSum>,
}

#[derive(Deserialize, Default)]
struct GqlKvActionDimension {
    #[serde(rename = "actionType")]
    action_type: Option<String>,
}

#[derive(Deserialize, Default)]
struct GqlKvSum {
    requests: Option<u64>,
}

#[derive(Deserialize, Default)]
struct GqlAnalyticsEngineItem {
    count: Option<u64>,
}

pub struct FetchedAnalytics {
    pub workers_requests: u64,
    pub workers_cpu_time_us: u64,
    pub workers_errors: u64,
    pub d1_rows_read: u64,
    pub d1_rows_written: u64,
    pub d1_read_queries: u64,
    pub d1_write_queries: u64,
    pub r2_class_a_operations: u64,
    pub r2_class_b_operations: u64,
    pub r2_operations: u64,
    pub kv_read_operations: u64,
    pub kv_write_operations: u64,
    pub analytics_engine_points: u64,
    pub daily_records: Vec<(String, String, String, f64)>,
}

pub fn query_analytics(
    client: &reqwest::blocking::Client,
    account_id: &str,
    api_token: &str,
    period: &BillingCycleInfo,
) -> Result<FetchedAnalytics, String> {
    let query = r#"
query AccountAnalytics($accountTag: String!, $start: Time!, $end: Time!, $startDate: Date!, $endDate: Date!) {
  viewer {
    accounts(filter: { accountTag: $accountTag }) {
      workersOverview: workersInvocationsAdaptive(filter: { datetime_geq: $start, datetime_lt: $end }, limit: 10000) {
        sum { requests errors cpuTimeUs }
      }
      workersDaily: workersInvocationsAdaptive(filter: { datetime_geq: $start, datetime_lt: $end }, limit: 10000, orderBy: [date_ASC]) {
        dimensions { date }
        sum { requests errors cpuTimeUs }
      }
      d1Overview: d1AnalyticsAdaptiveGroups(filter: { date_geq: $startDate, date_leq: $endDate }, limit: 10000) {
        sum { readQueries writeQueries rowsRead rowsWritten }
      }
      d1Daily: d1AnalyticsAdaptiveGroups(filter: { date_geq: $startDate, date_leq: $endDate }, limit: 10000, orderBy: [date_ASC]) {
        dimensions { date }
        sum { readQueries writeQueries rowsRead rowsWritten }
      }
      r2Overview: r2OperationsAdaptiveGroups(filter: { date_geq: $startDate, date_leq: $endDate }, limit: 10000) {
        dimensions { actionType }
        sum { requests responseObjectSize }
      }
      r2Daily: r2OperationsAdaptiveGroups(filter: { date_geq: $startDate, date_leq: $endDate }, limit: 10000, orderBy: [date_ASC]) {
        dimensions { date }
        sum { requests }
      }
      kvOperations: kvOperationsAdaptiveGroups(filter: { date_geq: $startDate, date_leq: $endDate }, limit: 100) {
        dimensions { actionType }
        sum { requests }
      }
      analyticsEngine: workersAnalyticsEngineAdaptiveGroups(filter: { date_geq: $startDate, date_leq: $endDate }, limit: 100) {
        count
      }
    }
  }
}
"#;

    let variables = serde_json::json!({
        "accountTag": account_id,
        "start": period.start,
        "end": period.end,
        "startDate": period.start_date,
        "endDate": period.end_date,
    });

    let body = serde_json::json!({
        "query": query,
        "variables": variables,
    });

    let resp = client
        .post("https://api.cloudflare.com/client/v4/graphql")
        .header("Authorization", format!("Bearer {api_token}"))
        .json(&body)
        .send()
        .map_err(|e| format!("GraphQL 请求失败：{e}"))?;

    if !resp.status().is_success() {
        return Err(format!("GraphQL 返回 HTTP 错误：{}", resp.status()));
    }

    let parsed: GqlResponse = resp
        .json()
        .map_err(|e| format!("GraphQL 响应解析失败：{e}"))?;

    if let Some(errors) = &parsed.errors {
        if !errors.is_empty() && parsed.data.is_none() {
            let err_msg = errors
                .iter()
                .map(|e| e.message.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            return Err(format!("GraphQL 接口错误：{err_msg}"));
        }
    }

    let mut analytics = FetchedAnalytics {
        workers_requests: 0,
        workers_cpu_time_us: 0,
        workers_errors: 0,
        d1_rows_read: 0,
        d1_rows_written: 0,
        d1_read_queries: 0,
        d1_write_queries: 0,
        r2_class_a_operations: 0,
        r2_class_b_operations: 0,
        r2_operations: 0,
        kv_read_operations: 0,
        kv_write_operations: 0,
        analytics_engine_points: 0,
        daily_records: Vec::new(),
    };

    let node = parsed
        .data
        .and_then(|d| d.viewer)
        .and_then(|v| v.accounts)
        .and_then(|mut a| if !a.is_empty() { Some(a.remove(0)) } else { None });

    let Some(node) = node else {
        return Ok(analytics);
    };

    // Workers Overview
    if let Some(overview) = node.workers_overview {
        for item in overview {
            if let Some(sum) = item.sum {
                analytics.workers_requests += sum.requests.unwrap_or(0);
                analytics.workers_errors += sum.errors.unwrap_or(0);
                analytics.workers_cpu_time_us += sum.cpu_time_us.unwrap_or(0);
            }
        }
    }

    // Workers Daily
    if let Some(daily) = node.workers_daily {
        for item in daily {
            if let (Some(dim), Some(sum)) = (item.dimensions, item.sum) {
                if let Some(date) = dim.date {
                    let req = sum.requests.unwrap_or(0);
                    let err = sum.errors.unwrap_or(0);
                    let cpu = sum.cpu_time_us.unwrap_or(0);
                    analytics.daily_records.push((date.clone(), "workers".into(), "requests".into(), req as f64));
                    analytics.daily_records.push((date.clone(), "workers".into(), "errors".into(), err as f64));
                    analytics.daily_records.push((date, "workers".into(), "cpu_time_us".into(), cpu as f64));
                }
            }
        }
    }

    // D1 Overview
    if let Some(overview) = node.d1_overview {
        for item in overview {
            if let Some(sum) = item.sum {
                analytics.d1_rows_read += sum.rows_read.unwrap_or(0);
                analytics.d1_rows_written += sum.rows_written.unwrap_or(0);
                analytics.d1_read_queries += sum.read_queries.unwrap_or(0);
                analytics.d1_write_queries += sum.write_queries.unwrap_or(0);
            }
        }
    }

    // D1 Daily
    if let Some(daily) = node.d1_daily {
        for item in daily {
            if let (Some(dim), Some(sum)) = (item.dimensions, item.sum) {
                if let Some(date) = dim.date {
                    let r = sum.rows_read.unwrap_or(0);
                    let w = sum.rows_written.unwrap_or(0);
                    analytics.daily_records.push((date.clone(), "d1".into(), "rows_read".into(), r as f64));
                    analytics.daily_records.push((date, "d1".into(), "rows_written".into(), w as f64));
                }
            }
        }
    }

    // R2 Overview
    if let Some(overview) = node.r2_overview {
        for item in overview {
            let reqs = item.sum.as_ref().and_then(|s| s.requests).unwrap_or(0);
            analytics.r2_operations += reqs;
            let action = item
                .dimensions
                .as_ref()
                .and_then(|d| d.action_type.as_deref())
                .unwrap_or("");
            match action {
                "GetObject" | "HeadObject" | "HeadBucket" => {
                    analytics.r2_class_b_operations += reqs;
                }
                _ => {
                    analytics.r2_class_a_operations += reqs;
                }
            }
        }
    }

    // R2 Daily
    if let Some(daily) = node.r2_daily {
        for item in daily {
            if let (Some(dim), Some(sum)) = (item.dimensions, item.sum) {
                if let Some(date) = dim.date {
                    let ops = sum.requests.unwrap_or(0);
                    analytics.daily_records.push((date, "r2".into(), "operations".into(), ops as f64));
                }
            }
        }
    }

    // KV Operations
    if let Some(kv_ops) = node.kv_operations {
        for item in kv_ops {
            let reqs = item.sum.as_ref().and_then(|s| s.requests).unwrap_or(0);
            let action = item
                .dimensions
                .as_ref()
                .and_then(|d| d.action_type.as_deref())
                .unwrap_or("");
            match action {
                "read" | "get" => analytics.kv_read_operations += reqs,
                _ => analytics.kv_write_operations += reqs,
            }
        }
    }

    // Analytics Engine
    if let Some(ae_items) = node.analytics_engine {
        for item in ae_items {
            analytics.analytics_engine_points += item.count.unwrap_or(0);
        }
    }

    Ok(analytics)
}

// ---------------------------------------------------------------------------
// Database Operations
// ---------------------------------------------------------------------------

pub fn persist_daily_metrics(
    conn: &mut Connection,
    account_id: &str,
    records: &[(String, String, String, f64)],
    now_ms: i64,
) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO cf_daily_usage(account_id, date, service, metric, value, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for (date, service, metric, val) in records {
            stmt.execute(rusqlite::params![
                account_id, date, service, metric, val, now_ms
            ])?;
        }
    }
    tx.commit()
}

pub fn query_monthly_history(
    conn: &Connection,
    account_id: &str,
) -> rusqlite::Result<Vec<MonthlyUsage>> {
    let mut stmt = conn.prepare(
        "SELECT strftime('%Y-%m', date) AS month, service, metric, SUM(value) AS total
         FROM cf_daily_usage
         WHERE account_id = ?1
         GROUP BY month, service, metric
         ORDER BY month DESC",
    )?;

    let mut months_map: BTreeMap<String, MonthlyUsage> = BTreeMap::new();
    let rows = stmt.query_map([account_id], |row| {
        let month: String = row.get(0)?;
        let service: String = row.get(1)?;
        let metric: String = row.get(2)?;
        let total: f64 = row.get(3)?;
        Ok((month, service, metric, total))
    })?;

    for row in rows.flatten() {
        let (month, service, metric, val) = row;
        let entry = months_map.entry(month.clone()).or_insert_with(|| MonthlyUsage {
            month,
            ..Default::default()
        });
        match (service.as_str(), metric.as_str()) {
            ("workers", "requests") => entry.workers_requests = val.round() as u64,
            ("workers", "cpu_time_us") => entry.workers_cpu_time_us = val.round() as u64,
            ("workers", "errors") => entry.workers_errors = val.round() as u64,
            ("d1", "rows_read") => entry.d1_rows_read = val.round() as u64,
            ("d1", "rows_written") => entry.d1_rows_written = val.round() as u64,
            ("r2", "operations") => entry.r2_operations = val.round() as u64,
            _ => {}
        }
    }

    let mut result: Vec<MonthlyUsage> = months_map.into_values().collect();
    result.sort_by(|a, b| b.month.cmp(&a.month));
    Ok(result)
}

pub fn query_daily_trend(
    conn: &Connection,
    account_id: &str,
    limit_days: usize,
) -> rusqlite::Result<Vec<DailyUsage>> {
    let mut stmt = conn.prepare(
        "SELECT date, service, metric, value
         FROM cf_daily_usage
         WHERE account_id = ?1
         ORDER BY date DESC",
    )?;

    let mut days_map: BTreeMap<String, DailyUsage> = BTreeMap::new();
    let rows = stmt.query_map([account_id], |row| {
        let date: String = row.get(0)?;
        let service: String = row.get(1)?;
        let metric: String = row.get(2)?;
        let val: f64 = row.get(3)?;
        Ok((date, service, metric, val))
    })?;

    for row in rows.flatten() {
        let (date, service, metric, val) = row;
        let entry = days_map.entry(date.clone()).or_insert_with(|| DailyUsage {
            date,
            ..Default::default()
        });
        match (service.as_str(), metric.as_str()) {
            ("workers", "requests") => entry.workers_requests = val.round() as u64,
            ("workers", "cpu_time_us") => entry.workers_cpu_time_us = val.round() as u64,
            ("workers", "errors") => entry.workers_errors = val.round() as u64,
            ("d1", "rows_read") => entry.d1_rows_read = val.round() as u64,
            ("d1", "rows_written") => entry.d1_rows_written = val.round() as u64,
            ("r2", "operations") => entry.r2_operations = val.round() as u64,
            _ => {}
        }
    }

    let mut result: Vec<DailyUsage> = days_map.into_values().collect();
    result.sort_by(|a, b| a.date.cmp(&b.date));
    if result.len() > limit_days {
        result = result.split_off(result.len() - limit_days);
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// Report Construction & Caching Engine
// ---------------------------------------------------------------------------

fn make_metric(
    name: &str,
    val: f64,
    quota: f64,
    unit: &str,
    format_val: impl Fn(f64) -> String,
    format_q: impl Fn(f64) -> String,
) -> MetricItem {
    let pct = if quota > 0.0 {
        (val / quota) * 100.0
    } else {
        0.0
    };
    let rem = (quota - val).max(0.0);
    let status = if pct >= 90.0 {
        "danger".to_string()
    } else if pct >= 75.0 {
        "warning".to_string()
    } else {
        "normal".to_string()
    };

    MetricItem {
        name: name.to_string(),
        value: val,
        quota,
        remaining: rem,
        percent: pct,
        formatted_value: format_val(val),
        formatted_quota: format_q(quota),
        formatted_remaining: format_val(rem),
        unit: unit.to_string(),
        status,
        progress_bar: format_progress_bar(pct, 10),
    }
}

pub fn build_current_period_summary(
    quotas: &CloudflareQuotas,
    workers_requests: u64,
    workers_cpu_time_us: u64,
    workers_errors: u64,
    d1_rows_read: u64,
    d1_rows_written: u64,
    d1_read_queries: u64,
    d1_write_queries: u64,
    d1_storage_bytes: u64,
    d1_databases_count: u64,
    r2_class_a_operations: u64,
    r2_class_b_operations: u64,
    r2_operations: u64,
    r2_storage_bytes: u64,
    r2_buckets_count: u64,
    r2_objects_count: u64,
    kv_read_operations: u64,
    kv_write_operations: u64,
    kv_namespaces_count: u64,
    analytics_engine_points: u64,
) -> CurrentPeriodSummary {
    let workers_requests_item = make_metric(
        "Workers 请求次数",
        workers_requests as f64,
        quotas.workers_requests,
        "次",
        |v| format_compact_number(v),
        |q| format_compact_number(q),
    );

    let workers_cpu_time_item = make_metric(
        "Workers CPU 耗时",
        workers_cpu_time_us as f64,
        quotas.workers_cpu_time_us,
        "秒",
        |v| format_cpu_time(v),
        |q| format_cpu_time(q),
    );

    let err_pct = if workers_requests > 0 {
        (workers_errors as f64 / workers_requests as f64) * 100.0
    } else {
        0.0
    };
    let workers_errors_item = MetricItem {
        name: "Workers 错误与稳定性".into(),
        value: workers_errors as f64,
        quota: 0.0,
        remaining: 0.0,
        percent: err_pct,
        formatted_value: format_with_commas(workers_errors),
        formatted_quota: "无错误额度".into(),
        formatted_remaining: format!("{:.2}% 成功率", (100.0 - err_pct).max(0.0)),
        unit: "次".into(),
        status: if err_pct > 1.0 { "warning".into() } else { "normal".into() },
        progress_bar: "".into(),
    };

    // D1
    let d1_rows_read_item = make_metric(
        "D1 读取行数",
        d1_rows_read as f64,
        quotas.d1_rows_read,
        "行",
        |v| format_compact_number(v),
        |q| format_compact_number(q),
    );

    let d1_rows_written_item = make_metric(
        "D1 写入行数",
        d1_rows_written as f64,
        quotas.d1_rows_written,
        "行",
        |v| format_compact_number(v),
        |q| format_compact_number(q),
    );

    let d1_storage_item = make_metric(
        "D1 存储容量",
        d1_storage_bytes as f64,
        quotas.d1_storage_bytes,
        "",
        |v| format_bytes(v),
        |q| format_bytes(q),
    );

    // R2
    let r2_class_a_item = make_metric(
        "R2 Class A 操作 (写/改/列)",
        r2_class_a_operations as f64,
        quotas.r2_class_a,
        "次",
        |v| format_compact_number(v),
        |q| format_compact_number(q),
    );

    let r2_class_b_item = make_metric(
        "R2 Class B 操作 (读/下载)",
        r2_class_b_operations as f64,
        quotas.r2_class_b,
        "次",
        |v| format_compact_number(v),
        |q| format_compact_number(q),
    );

    let r2_operations_item = make_metric(
        "R2 总操作数",
        r2_operations as f64,
        quotas.r2_operations,
        "次",
        |v| format_compact_number(v),
        |q| format_compact_number(q),
    );

    let r2_storage_item = make_metric(
        "R2 存储桶容量",
        r2_storage_bytes as f64,
        quotas.r2_storage_bytes,
        "",
        |v| format_bytes(v),
        |q| format_bytes(q),
    );

    // KV
    let kv_read_item = make_metric(
        "KV 读操作",
        kv_read_operations as f64,
        quotas.kv_read_operations,
        "次",
        |v| format_compact_number(v),
        |q| format_compact_number(q),
    );

    let kv_write_item = make_metric(
        "KV 写/删/列操作",
        kv_write_operations as f64,
        quotas.kv_write_operations,
        "次",
        |v| format_compact_number(v),
        |q| format_compact_number(q),
    );

    let kv_storage_item = make_metric(
        "KV 存储容量",
        0.0,
        quotas.kv_storage_bytes,
        "",
        |v| format_bytes(v),
        |q| format_bytes(q),
    );

    // Observability
    let ae_item = make_metric(
        "Analytics Engine 数据点",
        analytics_engine_points as f64,
        quotas.analytics_engine_points,
        "点",
        |v| format_compact_number(v),
        |q| format_compact_number(q),
    );
    let obs_events_item = make_metric(
        "Observability 日志事件",
        workers_requests as f64,
        quotas.observability_events,
        "次",
        |v| format_compact_number(v),
        |q| format_compact_number(q),
    );


    CurrentPeriodSummary {
        workers_requests: workers_requests_item,
        workers_cpu_time: workers_cpu_time_item,
        workers_errors: workers_errors_item,
        d1_rows_read: d1_rows_read_item,
        d1_rows_written: d1_rows_written_item,
        d1_storage: d1_storage_item,
        d1_read_queries,
        d1_write_queries,
        d1_databases_count,
        r2_class_a_operations: r2_class_a_item,
        r2_class_b_operations: r2_class_b_item,
        r2_operations: r2_operations_item,
        r2_storage: r2_storage_item,
        r2_buckets_count,
        r2_objects_count,
        kv_read_operations: kv_read_item,
        kv_write_operations: kv_write_item,
        kv_storage: kv_storage_item,
        kv_namespaces_count,
        observability_events: obs_events_item,
        analytics_engine_points: ae_item,
        subrequests_limit: 50,
        live_tail_limit: 2,
        logpush_jobs_limit: 4,
        cron_triggers_limit: 5,
    }
}

pub fn report(
    database: &Path,
    config: &CloudflareConfig,
    force: bool,
) -> Result<CloudflareReport, String> {
    if !config.is_configured() {
        return Err("未配置 Cloudflare Account ID 或 API Token".to_string());
    }

    let account_id = config.account_id.as_deref().unwrap();
    let api_token = config.api_token.as_deref().unwrap();
    let quotas = CloudflareQuotas::for_plan(&config.plan);

    let mut conn = crate::db::open(database).map_err(|e| format!("无法打开数据库：{e}"))?;

    let now_ms = Utc::now().timestamp_millis();
    const CACHE_TTL_MS: i64 = 15 * 60 * 1000; // 15 minutes

    // Check cached state
    let cached_state: Option<(String, String, i64, String)> = conn
        .query_row(
            "SELECT period_start, period_end, last_synced_at, cached_summary_json
             FROM cf_sync_state WHERE account_id = ?1",
            [account_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .ok();

    if !force {
        if let Some((_, _, last_synced_at, cached_json)) = &cached_state {
            if now_ms - *last_synced_at < CACHE_TTL_MS {
                if let Ok(mut rep) = serde_json::from_str::<CloudflareReport>(cached_json) {
                    rep.stale = false;
                    rep.error = None;
                    return Ok(rep);
                }
            }
        }
    }

    // Attempt remote sync
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| format!("HTTP 客户端初始化失败：{e}"))?;

    let period = fetch_billing_period(&client, account_id, api_token, config.billing_day, Utc::now());
    let storage = fetch_storage_details(&client, account_id, api_token);

    match query_analytics(&client, account_id, api_token, &period) {
        Ok(analytics) => {
            let _ = persist_daily_metrics(&mut conn, account_id, &analytics.daily_records, now_ms);

            let monthly_history = query_monthly_history(&conn, account_id).unwrap_or_default();
            let daily_trend = query_daily_trend(&conn, account_id, 30).unwrap_or_default();

            let summary = build_current_period_summary(
                &quotas,
                analytics.workers_requests,
                analytics.workers_cpu_time_us,
                analytics.workers_errors,
                analytics.d1_rows_read,
                analytics.d1_rows_written,
                analytics.d1_read_queries,
                analytics.d1_write_queries,
                storage.d1_total_bytes,
                storage.d1_databases_count,
                analytics.r2_class_a_operations,
                analytics.r2_class_b_operations,
                analytics.r2_operations,
                storage.r2_total_bytes,
                storage.r2_buckets_count,
                storage.r2_objects_count,
                analytics.kv_read_operations,
                analytics.kv_write_operations,
                storage.kv_namespaces_count,
                analytics.analytics_engine_points,
            );

            let ascii_card = generate_ascii_card(&summary);

            let report = CloudflareReport {
                configured: true,
                plan: config.plan.clone(),
                billing_cycle: period.clone(),
                current_period: summary,
                ascii_card,
                daily_trend,
                monthly_history,
                stale: false,
                error: None,
                synced_at: now_ms,
            };

            if let Ok(json) = serde_json::to_string(&report) {
                let _ = conn.execute(
                    "INSERT OR REPLACE INTO cf_sync_state(account_id, period_start, period_end, last_synced_at, cached_summary_json)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![account_id, period.start, period.end, now_ms, json],
                );
            }

            Ok(report)
        }
        Err(err) => {
            if let Some((_, _, _, cached_json)) = cached_state {
                if let Ok(mut rep) = serde_json::from_str::<CloudflareReport>(&cached_json) {
                    rep.stale = true;
                    rep.error = Some(err);
                    return Ok(rep);
                }
            }
            Err(err)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fallback_billing_period() {
        let dt = chrono::DateTime::parse_from_rfc3339("2026-03-15T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let period = fallback_billing_period(dt);
        assert_eq!(period.start, "2026-03-01T00:00:00Z");
        assert_eq!(period.end, "2026-04-01T00:00:00Z");
        assert_eq!(period.start_date, "2026-03-01");
        assert_eq!(period.end_date, "2026-04-01");
        assert!(period.is_fallback);
        assert!(period.days_remaining > 0);
    }

    #[test]
    fn test_billing_period_with_day() {
        let dt = chrono::DateTime::parse_from_rfc3339("2026-09-22T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let period = calculate_billing_period_with_day(dt, 11);
        assert_eq!(period.start_date, "2026-09-11");
        assert_eq!(period.end_date, "2026-10-11");
        assert!(!period.is_fallback);
        assert_eq!(period.days_remaining, 18);
        assert_eq!(period.elapsed_days, 11);
    }

    #[test]
    fn test_format_progress_bar() {
        assert_eq!(format_progress_bar(0.0, 10), "          ");
        assert_eq!(format_progress_bar(100.0, 10), "██████████");
        assert_eq!(format_progress_bar(32.4, 10), "███       ");
        assert_eq!(format_progress_bar(50.0, 10), "█████     ");
        assert_eq!(format_progress_bar(16.1, 10), "██        ");
    }

    #[test]
    fn test_format_compact_numbers() {
        assert_eq!(format_compact_number(3_240_000.0), "3.24M");
        assert_eq!(format_compact_number(11_300_000.0), "11.3M");
        assert_eq!(format_compact_number(8_720_000_000.0), "8.72B");
        assert_eq!(format_compact_number(284_000.0), "284K");
        assert_eq!(format_compact_number(1_203.0), "1,203");
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(50_143_232.0), "50.1 MB");
        assert_eq!(format_bytes(5_000_000_000.0), "5.00 GB");
        assert_eq!(format_bytes(838_212.0), "838.2 KB");
    }

    #[test]
    fn test_ascii_card_generation() {
        let quotas = CloudflareQuotas::for_plan("paid");
        let summary = build_current_period_summary(
            &quotas,
            3_240_000,
            48_200_000,
            1_203,
            8_720_000_000,
            11_300_000,
            1000,
            200,
            50_000_000,
            5,
            284_000,
            5000,
            289_000,
            800_000,
            2,
            8,
            1000,
            100,
            0,
            0,
        );
        let card = generate_ascii_card(&summary);
        assert!(card.contains("WORKERS"));
        assert!(card.contains("Requests"));
        assert!(card.contains("3.24M"));
        assert!(card.contains("32.4%"));
        assert!(card.contains("CPU Time"));
        assert!(card.contains("48.2 sec"));
        assert!(card.contains("0.2%"));
        assert!(card.contains("Errors"));
        assert!(card.contains("1,203"));
        assert!(card.contains("D1"));
        assert!(card.contains("Rows Read"));
        assert!(card.contains("8.72B"));
        assert!(card.contains("34.9%"));
        assert!(card.contains("Rows Written"));
        assert!(card.contains("11.3M"));
        assert!(card.contains("22.6%"));
        assert!(card.contains("R2"));
        assert!(card.contains("Operations"));
        assert!(card.contains("289K"));

        for line in card.lines() {
            assert_eq!(line.chars().count(), 47, "Line width mismatch: {}", line);
        }
    }

    #[test]
    fn test_sqlite_persistence_and_queries() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let db_path = tmp.path();
        let mut conn = crate::db::open(db_path).unwrap();

        let records = vec![
            ("2026-03-01".to_string(), "workers".to_string(), "requests".to_string(), 1000.0),
            ("2026-03-01".to_string(), "workers".to_string(), "cpu_time_us".to_string(), 5000.0),
            ("2026-03-02".to_string(), "workers".to_string(), "requests".to_string(), 2000.0),
            ("2026-03-02".to_string(), "d1".to_string(), "rows_read".to_string(), 30000.0),
            ("2026-03-02".to_string(), "r2".to_string(), "operations".to_string(), 500.0),
        ];

        persist_daily_metrics(&mut conn, "acc1", &records, 123456789).unwrap();

        let monthly = query_monthly_history(&conn, "acc1").unwrap();
        assert_eq!(monthly.len(), 1);
        assert_eq!(monthly[0].month, "2026-03");
        assert_eq!(monthly[0].workers_requests, 3000);
        assert_eq!(monthly[0].d1_rows_read, 30000);
        assert_eq!(monthly[0].r2_operations, 500);

        let daily = query_daily_trend(&conn, "acc1", 30).unwrap();
        assert_eq!(daily.len(), 2);
        assert_eq!(daily[0].date, "2026-03-01");
        assert_eq!(daily[0].workers_requests, 1000);
        assert_eq!(daily[1].date, "2026-03-02");
        assert_eq!(daily[1].workers_requests, 2000);
        assert_eq!(daily[1].d1_rows_read, 30000);
        assert_eq!(daily[1].r2_operations, 500);
    }

    #[test]
    fn test_graphql_response_parsing_and_idempotence() {
        let json = r#"{
            "data": {
                "viewer": {
                    "accounts": [{
                        "workersOverview": [{
                            "sum": { "requests": 3240000, "errors": 1203, "cpuTimeUs": 48200000 }
                        }],
                        "workersDaily": [{
                            "dimensions": { "date": "2026-03-01" },
                            "sum": { "requests": 1500000, "errors": 500, "cpuTimeUs": 24000000 }
                        }],
                        "d1Overview": [{
                            "sum": { "rowsRead": 8720000000, "rowsWritten": 11300000, "readQueries": 1000, "writeQueries": 200 }
                        }],
                        "d1Daily": [{
                            "dimensions": { "date": "2026-03-01" },
                            "sum": { "rowsRead": 4000000000, "rowsWritten": 5000000, "readQueries": 500, "writeQueries": 100 }
                        }],
                        "r2Overview": [{
                            "dimensions": { "actionType": "PutObject" },
                            "sum": { "requests": 284000, "responseObjectSize": 1024000 }
                        }],
                        "r2Daily": [{
                            "dimensions": { "date": "2026-03-01" },
                            "sum": { "requests": 140000 }
                        }]
                    }]
                }
            }
        }"#;
        let parsed: GqlResponse = serde_json::from_str(json).unwrap();
        assert!(parsed.data.is_some());

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let db_path = tmp.path();
        let mut conn = crate::db::open(db_path).unwrap();

        let records = vec![
            ("2026-03-01".to_string(), "workers".to_string(), "requests".to_string(), 1500000.0),
            ("2026-03-01".to_string(), "d1".to_string(), "rows_read".to_string(), 4000000000.0),
        ];
        persist_daily_metrics(&mut conn, "acc_test", &records, 1000).unwrap();
        let monthly1 = query_monthly_history(&conn, "acc_test").unwrap();
        assert_eq!(monthly1[0].workers_requests, 1500000);

        let records_update = vec![
            ("2026-03-01".to_string(), "workers".to_string(), "requests".to_string(), 1600000.0),
            ("2026-03-01".to_string(), "d1".to_string(), "rows_read".to_string(), 4500000000.0),
        ];
        persist_daily_metrics(&mut conn, "acc_test", &records_update, 2000).unwrap();
        let monthly2 = query_monthly_history(&conn, "acc_test").unwrap();
        assert_eq!(monthly2.len(), 1);
        assert_eq!(monthly2[0].workers_requests, 1600000);
        assert_eq!(monthly2[0].d1_rows_read, 4500000000);
    }
}
