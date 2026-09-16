//! 套餐额度：把「已用比例」样本换算成每个订阅套餐的每周额度上限。
//!
//! 四个订阅套餐分属三个「组」，同组的两份订阅额度相同、上限共享：
//! `codex`、`antigravity`（AGY + AGY2）、`grok`。组内上限由
//! 「本组在某个周期内的实际用量 ÷ 该周期的实际已用比例」推算，
//! 因此不需要知道套餐的标称额度，也不需要任何套餐特判。
//!
//! 两个口径要点（本机 2026-09-15 实测）：
//!
//! * **周期由套餐自己的重置时间决定**，不是自然周。omp 账本按 `resets_at`
//!   聚类（毫秒级抖动，容差 60 秒）。Codex 的 `resets_at` 会随使用整体后移，
//!   相邻周期的起点可以重叠：旧周期在被新周期接替的那一刻截断，
//!   否则新周期的用量会被算进旧周期，把推算值抬得离谱。
//! * **比例回落 = 同一周期内的额外赠送额度**。同一 `resets_at` 内比例从 0.9
//!   掉回 0.05 再涨到 0.4，说明额度提前回满：把回落前的峰值计入 Σf 后从新值
//!   继续累计（Σf = 1.3），上限自然升高，不需要「重置」特判。
//!
//! 比例样本的两个来源：omp 自带的按账户额度账本
//! （`~/.omp/agent/agent.db` 及每个 profile 的 `usage_history`）与 Grok CLI
//! 自己的账单日志（`~/.grok/logs/unified.jsonl`，omp 账本里没有 Grok）。

use chrono::{Local, TimeZone};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(test)]
mod tests;

/// 名义周期长度。Grok 的周期长度取账单日志里的 `currentPeriod`。
pub const WINDOW_DAYS: i64 = 7;

const DAY_MS: i64 = 86_400_000;
/// `resets_at` 的毫秒级抖动容差：实测同一周期最多上报过 621 个「不同」的
/// `resets_at`，其实都是同一时刻，不带容差会把一个周期拆成 621 个。
const CYCLE_TOLERANCE_MS: i64 = 60_000;
/// 同组两个账户的周期对齐容差（两份订阅的重置时刻实测相差几分钟）。
const GROUP_TOLERANCE_MS: i64 = 43_200_000;
/// 周期起点早于用量库首条记录时，`U` 会系统性偏小。缺口超过这个比例就整条丢弃。
const MAX_MISSING_RATIO: f64 = 0.10;

/// 比例样本的来源。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ledger {
    /// omp 自带额度账本 `usage_history`。
    Omp,
    /// Grok CLI 账单日志。
    GrokLog,
}

/// 一个订阅套餐。`provider`/`limit_id` 是它在额度账本里的定位；
/// `Ledger::GrokLog` 的两个字段为空串（样本来自日志文件，不查表）。
#[derive(Clone, Copy, Debug)]
pub struct PlanDef {
    pub id: &'static str,
    pub title: &'static str,
    pub group: &'static str,
    pub provider: &'static str,
    pub limit_id: &'static str,
    pub ledger: Ledger,
}

/// 四个套餐。标题与 TUI 的 `Source::title()` 保持一致。
pub const PLANS: [PlanDef; 4] = [
    PlanDef {
        id: "codex",
        title: "Codex · GPT",
        group: "codex",
        provider: "openai-codex",
        limit_id: "openai-codex:primary",
        ledger: Ledger::Omp,
    },
    PlanDef {
        id: "agy",
        title: "AGY",
        group: "antigravity",
        provider: "google-antigravity",
        limit_id: "google-antigravity:google:default:gemini-weekly",
        ledger: Ledger::Omp,
    },
    PlanDef {
        id: "agy2",
        title: "AGY2",
        group: "antigravity",
        provider: "google-antigravity",
        limit_id: "google-antigravity:google:default:gemini-weekly",
        ledger: Ledger::Omp,
    },
    PlanDef {
        id: "grok",
        title: "SuperGrok",
        group: "grok",
        provider: "",
        limit_id: "",
        ledger: Ledger::GrokLog,
    },
];

/// 组 → 组内用量过滤片段。
///
/// 全部是写死的常量片段，不含任何用户输入，因此拼进 SQL 是安全的。
/// codex CLI 的事件没有 provider 列（它自己就是 Codex 订阅），必须一并计入。
fn group_filter(group: &str) -> &'static str {
    match group {
        "codex" => "(provider = 'openai-codex' OR (source = 'codex' AND provider IS NULL))",
        "antigravity" => "provider = 'google-antigravity'",
        "grok" => "source = 'grok'",
        // 组名只来自 PLANS，未知组匹配不到任何行。
        _ => "0",
    }
}

/// 组，按 `PLANS` 里首次出现的顺序。
fn groups() -> Vec<&'static str> {
    let mut seen = Vec::new();
    for def in PLANS {
        if !seen.contains(&def.group) {
            seen.push(def.group);
        }
    }
    seen
}

/// 组内套餐 id，按 `PLANS` 顺序。
fn plans_of(group: &str) -> Vec<&'static str> {
    PLANS
        .iter()
        .filter(|def| def.group == group)
        .map(|def| def.id)
        .collect()
}

/// 组 → 比例样本的定位。未知组返回空定位（查不到任何行），不 panic。
fn ledger_of(group: &str) -> (Ledger, &'static str, &'static str) {
    match PLANS.iter().find(|def| def.group == group) {
        Some(def) => (def.ledger, def.provider, def.limit_id),
        None => (Ledger::Omp, "", ""),
    }
}

fn group_title(group: &str) -> &'static str {
    match group {
        "codex" => "Codex",
        "antigravity" => "AGY / AGY2（Antigravity）",
        _ => "SuperGrok",
    }
}

/// 网页需要的全部外部输入：两个 Antigravity home（用来识别 AGY 账户）。
#[derive(Clone, Debug)]
pub struct PlanOptions {
    pub agy_home: PathBuf,
    pub agy2_home: PathBuf,
}

impl Default for PlanOptions {
    fn default() -> Self {
        let home = crate::home_dir();
        Self {
            agy_home: home.join(".gemini"),
            agy2_home: home.join(".gemini2"),
        }
    }
}

/// 比例样本文件的位置。测试用临时目录构造，生产从 `$HOME` 推导。
#[derive(Clone, Debug)]
struct LedgerPaths {
    omp_dbs: Vec<PathBuf>,
    grok_log: PathBuf,
}

impl LedgerPaths {
    fn from_home(home: &Path) -> Self {
        let mut omp_dbs = vec![home.join(".omp/agent/agent.db")];
        let profiles = home.join(".omp/profiles");
        if let Ok(entries) = std::fs::read_dir(&profiles) {
            let mut extra: Vec<PathBuf> = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path().join("agent/agent.db"))
                .collect();
            extra.sort();
            omp_dbs.extend(extra);
        }
        Self {
            omp_dbs,
            grok_log: home.join(".grok/logs/unified.jsonl"),
        }
    }
}

/// 一条「已用比例」样本，两种来源归一化成同一个形状。
#[derive(Clone, Debug)]
struct Sample {
    account_key: String,
    label: String,
    email: Option<String>,
    recorded_at_ms: i64,
    /// 已用百分比（0..100+）。
    used_percent: f64,
    /// 名义重置时刻。
    resets_at_ms: i64,
    window_days: i64,
}

// ---------------------------------------------------------------------------
// 序列化结构（字段名即 JSON 名）
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct PlanReport {
    generated_at: String,
    timezone: String,
    /// 至少找到一处比例来源（omp 账本或 Grok 账单日志）。
    ledger_found: bool,
    accounts: Vec<AccountEntry>,
    plans: Vec<PlanEntry>,
    notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct AccountEntry {
    plan: &'static str,
    label: String,
    used_percent: Option<f64>,
    resolved: bool,
}

#[derive(Debug, Serialize)]
pub struct PlanEntry {
    id: &'static str,
    title: &'static str,
    group: &'static str,
    account: Option<String>,
    account_resolved: bool,
    period: Option<Period>,
    used_percent: Option<f64>,
    used_tokens: Option<i64>,
    used_cost_usd: Option<f64>,
    max_tokens: Option<i64>,
    max_cost_usd: Option<f64>,
    /// 该周期最新一条样本的本地时间，用来判断比例是否新鲜。
    sample_at: Option<String>,
    /// 周期尚未被接替、也未到名义终点。
    open: bool,
    /// 本组本周期的 Σf×100，说明同组共享上限。
    group_percent: Option<f64>,
    weekly: Vec<WeekPoint>,
}

#[derive(Debug, Serialize)]
pub struct Period {
    /// 周期起点（= `resets_at` − 周期长度），本地日期。
    start: String,
    /// 名义终点（= `resets_at`），本地日期。
    end: String,
    resets_at_ms: i64,
    days: i64,
}

#[derive(Debug, Serialize)]
pub struct WeekPoint {
    /// 周期起点，本地日期。
    start: String,
    /// 统计窗口终点（被下个周期接替时已截断），本地日期。
    end: String,
    /// 该周期名义重置时刻。
    resets_at_ms: i64,
    used_percent: Option<f64>,
    used_tokens: Option<i64>,
    used_cost_usd: Option<f64>,
    max_tokens: Option<i64>,
    max_cost_usd: Option<f64>,
    sample_at: String,
    open: bool,
    missing: f64,
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

/// 读用量库与两个比例来源，产出网页需要的整份报告。
pub fn report(usage_db: &Path, options: &PlanOptions) -> Result<PlanReport, String> {
    let connection = Connection::open_with_flags(
        usage_db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "无法读取统计数据库，请确认 collector 已运行且 HERDR_USAGE_DB 路径正确".to_string())?;
    connection
        .busy_timeout(Duration::from_secs(3))
        .map_err(|error| error.to_string())?;
    assemble(
        &connection,
        options,
        &LedgerPaths::from_home(&crate::home_dir()),
        chrono::Utc::now().timestamp_millis(),
    )
}

fn assemble(
    connection: &Connection,
    options: &PlanOptions,
    paths: &LedgerPaths,
    now_ms: i64,
) -> Result<PlanReport, String> {
    let (samples, ledger_found, notes) = read_samples(paths);
    let mut report = build(connection, &samples, options, now_ms, notes)?;
    report.ledger_found = ledger_found;
    Ok(report)
}

fn build(
    connection: &Connection,
    samples: &BTreeMap<&'static str, Vec<Sample>>,
    options: &PlanOptions,
    now_ms: i64,
    seed: Vec<String>,
) -> Result<PlanReport, String> {
    let mut notes = seed;
    let mut resolution: HashMap<&'static str, Option<String>> = HashMap::new();
    let mut infos_by_group: HashMap<&'static str, Vec<AccountInfo>> = HashMap::new();
    let mut cycles_by_group: HashMap<&'static str, Vec<GroupCycle>> = HashMap::new();
    let mut dropped = 0usize;

    for group in groups() {
        let group_samples: &[Sample] = samples.get(group).map(Vec::as_slice).unwrap_or(&[]);
        let infos = account_infos(group_samples);
        let assigned = resolve(group, &infos, options);
        if !group_samples.is_empty() && assigned.values().any(Option::is_none) {
            let missing: Vec<&'static str> = assigned
                .iter()
                .filter(|(_, account)| account.is_none())
                .map(|(id, _)| *id)
                .collect();
            notes.push(unresolved_note(group, &infos, &missing));
        }
        for (plan, account) in assigned {
            resolution.insert(plan, account);
        }
        if group_samples.is_empty() {
            let (ledger, provider, limit_id) = ledger_of(group);
            notes.push(match ledger {
                Ledger::Omp => format!(
                    "{} 没有额度比例样本（额度账本里没有 {provider} / {limit_id} 的记录）",
                    group_title(group)
                ),
                Ledger::GrokLog => format!(
                    "{} 没有额度比例样本（Grok CLI 不运行时账单日志不会更新）",
                    group_title(group)
                ),
            });
        }

        let first_event_ms = group_first_event(connection, group)?;
        let mut kept = Vec::new();
        for cycle in group_cycles(group_samples, now_ms) {
            match measure(connection, group, cycle, first_event_ms, now_ms)? {
                Some(cycle) => kept.push(cycle),
                None => dropped += 1,
            }
        }
        infos_by_group.insert(group, infos);
        cycles_by_group.insert(group, kept);
    }
    if dropped > 0 {
        notes.push(format!(
            "有 {dropped} 个历史周期的开头未被用量库覆盖（缺口 > {:.0}%），不参与推算",
            MAX_MISSING_RATIO * 100.0
        ));
    }
    if let Some(first) = first_event_all(connection)? {
        notes.push(format!(
            "用量库数据始于 {}；omp2（pro2 profile）的历史会话按采集器「不回填历史」规则未导入",
            local_stamp(first)
        ));
    }

    let info_for = |plan: &'static str| -> Option<&AccountInfo> {
        let group = PLANS.iter().find(|def| def.id == plan)?.group;
        let key = resolution.get(plan)?.as_deref()?;
        infos_by_group
            .get(group)?
            .iter()
            .find(|info| info.key == key)
    };

    let accounts = PLANS
        .iter()
        .map(|def| AccountEntry {
            plan: def.id,
            label: info_for(def.id)
                .map(|info| info.label.clone())
                .unwrap_or_else(|| "未配置".to_owned()),
            used_percent: info_for(def.id).map(|info| info.last_percent),
            resolved: info_for(def.id).is_some(),
        })
        .collect();

    let plans = PLANS
        .iter()
        .map(|def| {
            let cycles: &[GroupCycle] = cycles_by_group
                .get(def.group)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let account = resolution
                .get(def.id)
                .and_then(|value| value.as_deref())
                .map(str::to_owned);
            let weekly: Vec<WeekPoint> = cycles
                .iter()
                .map(|cycle| week_point(cycle, account.as_deref()))
                .collect();
            let last = cycles.last();
            let current = last.map(|cycle| week_point(cycle, account.as_deref()));
            PlanEntry {
                id: def.id,
                title: def.title,
                group: def.group,
                account: info_for(def.id).map(|info| info.label.clone()),
                account_resolved: account.is_some(),
                period: last.map(|cycle| Period {
                    start: local_date(cycle.start_ms),
                    end: local_date(cycle.end_ms),
                    resets_at_ms: cycle.end_ms,
                    days: cycle.window_days,
                }),
                used_percent: current.as_ref().and_then(|point| point.used_percent),
                used_tokens: current.as_ref().and_then(|point| point.used_tokens),
                used_cost_usd: current.as_ref().and_then(|point| point.used_cost_usd),
                max_tokens: current.as_ref().and_then(|point| point.max_tokens),
                max_cost_usd: current.as_ref().and_then(|point| point.max_cost_usd),
                sample_at: current.as_ref().map(|point| point.sample_at.clone()),
                open: current.as_ref().is_some_and(|point| point.open),
                group_percent: last.map(|cycle| cycle.sum_percent),
                weekly,
            }
        })
        .collect();

    Ok(PlanReport {
        generated_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        timezone: Local::now().format("%Z (UTC %:z)").to_string(),
        ledger_found: false,
        accounts,
        plans,
        notes,
    })
}

// ---------------------------------------------------------------------------
// 比例样本
// ---------------------------------------------------------------------------

fn read_samples(
    paths: &LedgerPaths,
) -> (BTreeMap<&'static str, Vec<Sample>>, bool, Vec<String>) {
    let mut ledger_found = false;
    let mut notes = Vec::new();
    let mut out: BTreeMap<&'static str, Vec<Sample>> = BTreeMap::new();
    for group in groups() {
        let (ledger, provider, limit_id) = ledger_of(group);
        let samples = match ledger {
            Ledger::Omp => read_omp_ledger(provider, limit_id, &paths.omp_dbs, &mut ledger_found),
            Ledger::GrokLog => read_grok_log(&paths.grok_log, &mut ledger_found),
        };
        out.insert(group, samples);
    }
    if !paths.omp_dbs.iter().any(|path| path.exists()) {
        notes.push(format!(
            "未找到额度账本 {}",
            paths
                .omp_dbs
                .first()
                .map(|path| path.display().to_string())
                .unwrap_or_default()
        ));
    }
    if !paths.grok_log.exists() {
        notes.push(format!("未找到 Grok 账单日志 {}", paths.grok_log.display()));
    }
    (out, ledger_found, notes)
}

/// omp 额度账本里某个 (provider, limit_id) 的全部样本。
///
/// 文件不存在、表不存在、查询失败都只是「这个库没有样本」：账本是 omp 的内部
/// 表，改表时页面应当降级提示，而不是把整页打崩。
fn read_omp_ledger(
    provider: &str,
    limit_id: &str,
    dbs: &[PathBuf],
    ledger_found: &mut bool,
) -> Vec<Sample> {
    let mut samples = Vec::new();
    for path in dbs {
        if !path.exists() {
            continue;
        }
        let Ok(connection) = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) else {
            continue;
        };
        let _ = connection.busy_timeout(Duration::from_secs(3));
        let Ok(mut statement) = connection.prepare(
            "SELECT email, account_key, used_fraction, resets_at, recorded_at
             FROM usage_history
             WHERE provider = ?1 AND limit_id = ?2
               AND used_fraction IS NOT NULL AND resets_at IS NOT NULL
             ORDER BY recorded_at",
        ) else {
            continue;
        };
        let Ok(rows) = statement.query_map([provider, limit_id], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        }) else {
            continue;
        };
        *ledger_found = true;
        for (email, account_key, used_fraction, resets_at, recorded_at) in
            rows.filter_map(Result::ok)
        {
            samples.push(Sample {
                label: account_label(&account_key, email.as_deref()),
                account_key,
                email,
                recorded_at_ms: recorded_at,
                used_percent: used_fraction * 100.0,
                resets_at_ms: resets_at,
                window_days: WINDOW_DAYS,
            });
        }
    }
    samples
}

/// Grok CLI 的账单行：`msg = "billing: fetched credits config"`。
///
/// 行里带 `ctx.config.creditUsagePercent` 与 `ctx.config.currentPeriod`，
/// 单位已是百分比、周期长度由 `start`/`end` 给出。实测 100 条里有 12 条缺
/// `creditUsagePercent`（跳过），日志被轮转时更早的周期就没有样本。
fn read_grok_log(path: &Path, ledger_found: &mut bool) -> Vec<Sample> {
    const MSG: &str = "billing: fetched credits config";
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    *ledger_found = true;
    let mut samples = Vec::new();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        if !line.contains(MSG) {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(used_percent) = value
            .pointer("/ctx/config/creditUsagePercent")
            .and_then(Value::as_f64)
        else {
            continue;
        };
        let start = value
            .pointer("/ctx/config/currentPeriod/start")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_ms);
        let end = value
            .pointer("/ctx/config/currentPeriod/end")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_ms);
        let (Some(start_ms), Some(end_ms)) = (start, end) else {
            continue;
        };
        let Some(recorded_at_ms) = value
            .get("ts")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_ms)
        else {
            continue;
        };
        let label = value
            .pointer("/ctx/subscriptionTier")
            .and_then(Value::as_str)
            .filter(|tier| !tier.is_empty())
            .unwrap_or("SuperGrok")
            .to_owned();
        samples.push(Sample {
            account_key: "grok".to_owned(),
            label,
            email: None,
            recorded_at_ms,
            used_percent,
            resets_at_ms: end_ms,
            window_days: ((end_ms - start_ms) / DAY_MS).max(1),
        });
    }
    samples
}

/// 有 email 用 email；否则取 `secret:` 之后的哈希（如 `290a7e337b809a0f`）。
fn account_label(account_key: &str, email: Option<&str>) -> String {
    if let Some(email) = email.filter(|email| !email.is_empty()) {
        return email.to_owned();
    }
    account_key
        .rsplit_once("secret:")
        .map(|(_, tail)| tail.to_owned())
        .unwrap_or_else(|| account_key.to_owned())
}

fn parse_rfc3339_ms(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|stamp| stamp.timestamp_millis())
}

// ---------------------------------------------------------------------------
// 账户归属
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct AccountInfo {
    key: String,
    label: String,
    email: Option<String>,
    /// 该账户最新一条样本的已用百分比。
    last_percent: f64,
    last_resets_at_ms: i64,
    last_recorded_ms: i64,
}

fn account_infos(samples: &[Sample]) -> Vec<AccountInfo> {
    let mut infos: BTreeMap<String, AccountInfo> = BTreeMap::new();
    for sample in samples {
        match infos.get_mut(&sample.account_key) {
            Some(info) if info.last_recorded_ms >= sample.recorded_at_ms => {}
            Some(info) => {
                info.last_percent = sample.used_percent;
                info.last_resets_at_ms = sample.resets_at_ms;
                info.last_recorded_ms = sample.recorded_at_ms;
            }
            None => {
                infos.insert(
                    sample.account_key.clone(),
                    AccountInfo {
                        key: sample.account_key.clone(),
                        label: sample.label.clone(),
                        email: sample.email.clone(),
                        last_percent: sample.used_percent,
                        last_resets_at_ms: sample.resets_at_ms,
                        last_recorded_ms: sample.recorded_at_ms,
                    },
                );
            }
        }
    }
    infos.into_values().collect()
}

/// 套餐 id → 该套餐对应的账户标识（`None` = 未解析）。
///
/// 只做自动识别；识别不出来时宁可留空，也不猜。
fn resolve(
    group: &str,
    infos: &[AccountInfo],
    options: &PlanOptions,
) -> BTreeMap<&'static str, Option<String>> {
    let plan_ids = plans_of(group);
    let mut assigned: BTreeMap<&'static str, Option<String>> =
        plan_ids.iter().map(|id| (*id, None)).collect();

    match group {
        "codex" => {
            // 该 provider 只有一个账户，取采样最新的那个。
            assigned.insert(
                "codex",
                infos
                    .iter()
                    .max_by_key(|info| info.last_recorded_ms)
                    .map(|info| info.key.clone()),
            );
        }
        "grok" => {
            assigned.insert("grok", infos.first().map(|info| info.key.clone()));
        }
        "antigravity" => {
            // `~/.gemini` 的 oauth 令牌里带 id_token，解出邮箱即 AGY 账户。
            if assigned["agy"].is_none() {
                if let Some(email) = token_email(&options.agy_home) {
                    if let Some(key) = match_email(infos, &email) {
                        assigned.insert("agy", Some(key));
                    }
                }
            }
            // 账本里恰好两个 Antigravity 账户时，另一个就是 AGY2。
            if infos.len() == 2 {
                let known = assigned["agy"].clone().or_else(|| assigned["agy2"].clone());
                if let Some(known) = known {
                    if let Some(other) = infos.iter().find(|info| info.key != known) {
                        if assigned["agy"].is_none() {
                            assigned.insert("agy", Some(other.key.clone()));
                        } else if assigned["agy2"].is_none() {
                            assigned.insert("agy2", Some(other.key.clone()));
                        }
                    }
                }
            }
        }
        _ => {}
    }
    assigned
}

fn match_email(infos: &[AccountInfo], value: &str) -> Option<String> {
    infos
        .iter()
        .find(|info| {
            info.email
                .as_deref()
                .is_some_and(|email| email.eq_ignore_ascii_case(value.trim()))
        })
        .map(|info| info.key.clone())
}

/// 降级提示：解不出账户归属时，列出本组当前的候选账户，而不是猜一个。
fn unresolved_note(group: &str, infos: &[AccountInfo], missing: &[&'static str]) -> String {
    let current: Vec<&AccountInfo> = match infos.iter().map(|info| info.last_resets_at_ms).max() {
        Some(latest) => infos
            .iter()
            .filter(|info| latest - info.last_resets_at_ms <= GROUP_TOLERANCE_MS)
            .collect(),
        None => Vec::new(),
    };
    let list = current
        .iter()
        .map(|info| format!("{}（当前周 {:.1}%）", info.label, info.last_percent))
        .collect::<Vec<_>>()
        .join("、");
    let reason = match group {
        "antigravity" => "解不出 ~/.gemini 的 id_token 邮箱",
        _ => "无法自动识别",
    };
    format!(
        "{} 的账户归属未解析（{reason}）：{} 未定；候选账户 {}",
        group_title(group),
        missing.join("/"),
        if list.is_empty() {
            "（暂无）".to_owned()
        } else {
            list
        },
    )
}

/// 取 `~/.gemini/antigravity-cli/antigravity-oauth-token` 里 `id_token` 的
/// `email` 声明。不校验签名，只把它当作「这台机器上 AGY 用哪个账户」的线索。
fn token_email(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(home.join("antigravity-cli/antigravity-oauth-token")).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let token = value.get("id_token").and_then(Value::as_str)?;
    jwt_email(token)
}

/// JWT 的 payload 段（base64url）里的 `email` 声明。
fn jwt_email(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64url_decode(payload)?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("email")
        .and_then(Value::as_str)
        .filter(|email| !email.is_empty())
        .map(str::to_owned)
}

/// 手写 base64url 解码，避免为一行代码新增依赖。
fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3 + 3);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// 周期切分
// ---------------------------------------------------------------------------

/// 一个账户在自己的 `resets_at` 序列上的一个周期。
#[derive(Clone, Debug)]
struct Cycle {
    start_ms: i64,
    end_ms: i64,
    window_days: i64,
    /// Σf×100：本周期内按时间顺序累加的「单调上升段」最大值。
    sum_percent: f64,
    last_percent: f64,
    last_sample_ms: i64,
    /// 截断后的统计窗口终点。
    effective_end_ms: i64,
}

/// 多账户对齐后的组周期。同组两份订阅共享 `max_*`。
#[derive(Clone, Debug)]
struct GroupCycle {
    start_ms: i64,
    end_ms: i64,
    window_days: i64,
    effective_end_ms: i64,
    sum_percent: f64,
    sample_ms: i64,
    missing: f64,
    /// 周期尚未被接替、也未到名义终点（即统计窗口仍在增长）。
    open: bool,
    max_cost_usd: Option<f64>,
    max_tokens: Option<i64>,
    members: Vec<(String, Cycle)>,
}

impl GroupCycle {
    fn push(&mut self, account: &str, cycle: &Cycle) {
        if self.members.is_empty() {
            self.start_ms = cycle.start_ms;
            self.end_ms = cycle.end_ms;
            self.window_days = cycle.window_days;
            self.effective_end_ms = cycle.effective_end_ms;
            self.sample_ms = cycle.last_sample_ms;
        } else {
            // 组周期的终点取最早者，起点仍按「终点 − 周期长度」推导。
            self.end_ms = self.end_ms.min(cycle.end_ms);
            self.start_ms = self.end_ms - self.window_days * DAY_MS;
            self.effective_end_ms = self.effective_end_ms.min(cycle.effective_end_ms);
            self.sample_ms = self.sample_ms.max(cycle.last_sample_ms);
        }
        self.sum_percent += cycle.sum_percent;
        self.members.push((account.to_owned(), cycle.clone()));
    }

    fn percent_of(&self, account: &str) -> Option<f64> {
        self.members
            .iter()
            .find(|(key, _)| key == account)
            .map(|(_, cycle)| cycle.last_percent)
    }
}

/// 每个账户的周期序列，已按起点升序并完成截断。
fn account_cycles(samples: &[Sample], now_ms: i64) -> BTreeMap<String, Vec<Cycle>> {
    let mut by_account: BTreeMap<String, Vec<&Sample>> = BTreeMap::new();
    for sample in samples {
        by_account
            .entry(sample.account_key.clone())
            .or_default()
            .push(sample);
    }
    let mut out = BTreeMap::new();
    for (account, mut list) in by_account {
        list.sort_by_key(|sample| sample.recorded_at_ms);
        // 按 resets_at 聚类，毫秒级抖动视为同一个周期。
        let mut clusters: Vec<(i64, Vec<&Sample>)> = Vec::new();
        for sample in list {
            match clusters.last_mut() {
                Some((anchor, bucket))
                    if (sample.resets_at_ms - *anchor).abs() <= CYCLE_TOLERANCE_MS =>
                {
                    bucket.push(sample);
                }
                _ => clusters.push((sample.resets_at_ms, vec![sample])),
            }
        }
        let mut cycles = Vec::new();
        for (_, bucket) in clusters {
            // 同一周期取最后一次上报值作为规范值。
            let canonical = bucket
                .iter()
                .max_by_key(|sample| sample.recorded_at_ms)
                .copied()
                .expect("聚类非空");
            let mut peak = 0.0f64;
            let mut total = 0.0f64;
            for sample in &bucket {
                // 比例回落 = 额度提前回满：把回落前的峰值计入 Σf，再从新值累加。
                if sample.used_percent < peak - 0.01 {
                    total += peak;
                    peak = 0.0;
                }
                peak = peak.max(sample.used_percent);
            }
            total += peak;
            cycles.push(Cycle {
                start_ms: canonical.resets_at_ms - canonical.window_days * DAY_MS,
                end_ms: canonical.resets_at_ms,
                window_days: canonical.window_days,
                sum_percent: total,
                last_percent: canonical.used_percent,
                last_sample_ms: canonical.recorded_at_ms,
                effective_end_ms: canonical.resets_at_ms.min(now_ms),
            });
        }
        cycles.sort_by_key(|cycle| cycle.start_ms);
        // 被下个周期接替时，本周期只统计到自己被接替的那一刻。
        for index in 0..cycles.len() {
            if let Some(next) = cycles.get(index + 1) {
                let next_start = next.start_ms;
                cycles[index].effective_end_ms = cycles[index].effective_end_ms.min(next_start);
            }
        }
        out.insert(account, cycles);
    }
    out
}

/// 组周期：各账户周期按 `end_ms` 相差 ≤ 12 小时对齐，同一账户在一次对齐里
/// 最多出现一次（Codex 的相邻周期起点可以重叠，绝不能被并成一个）。
fn group_cycles(samples: &[Sample], now_ms: i64) -> Vec<GroupCycle> {
    let per_account = account_cycles(samples, now_ms);
    let mut items: Vec<(&String, &Cycle)> = per_account
        .iter()
        .flat_map(|(account, cycles)| cycles.iter().map(move |cycle| (account, cycle)))
        .collect();
    items.sort_by_key(|(_, cycle)| cycle.end_ms);

    let mut groups: Vec<GroupCycle> = Vec::new();
    for (account, cycle) in items {
        if let Some(last) = groups.last_mut() {
            let aligned = cycle.end_ms.saturating_sub(last.end_ms) <= GROUP_TOLERANCE_MS;
            let already_in = last.members.iter().any(|(key, _)| key == account);
            if aligned && !already_in {
                last.push(account, cycle);
                continue;
            }
        }
        let mut fresh = GroupCycle {
            start_ms: 0,
            end_ms: 0,
            window_days: WINDOW_DAYS,
            effective_end_ms: 0,
            sum_percent: 0.0,
            sample_ms: 0,
            missing: 0.0,
            open: false,
            max_cost_usd: None,
            max_tokens: None,
            members: Vec::new(),
        };
        fresh.push(account, cycle);
        groups.push(fresh);
    }
    groups
}

// ---------------------------------------------------------------------------
// 推算
// ---------------------------------------------------------------------------

/// 用组周期窗口内的实际用量除以本组 Σf 得到上限；缺口太大或没有用量时放弃推算。
/// 保留下来的周期带着推算结果返回，丢弃时返回 `None`。
fn measure(
    connection: &Connection,
    group: &str,
    mut cycle: GroupCycle,
    first_event_ms: Option<i64>,
    now_ms: i64,
) -> Result<Option<GroupCycle>, String> {
    let start = cycle.start_ms;
    // 进行中的周期统计到当前时刻；被接替的周期统计到自己被接替的那一刻。
    let end = cycle.effective_end_ms.clamp(start, now_ms);
    if end <= start {
        return Ok(None);
    }
    cycle.effective_end_ms = end;
    cycle.open = end >= now_ms;
    cycle.missing = match first_event_ms {
        Some(first) => (((first - start) as f64) / ((end - start) as f64)).clamp(0.0, 1.0),
        None => 1.0,
    };
    if cycle.missing > MAX_MISSING_RATIO {
        return Ok(None);
    }
    let (cost_usd, tokens) = group_usage(connection, group, start, end)?;
    let fraction = cycle.sum_percent / 100.0;
    // Σf ≈ 0 或没有任何用量时无法反推，页面对应字段留空。
    if fraction > 1e-9 && (cost_usd != 0.0 || tokens != 0) {
        cycle.max_cost_usd = Some(cost_usd / fraction);
        cycle.max_tokens = Some((tokens as f64 / fraction).round() as i64);
    }
    Ok(Some(cycle))
}

fn group_usage(
    connection: &Connection,
    group: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<(f64, i64), String> {
    let sql = format!(
        "SELECT COALESCE(SUM(cost_usd), 0), COALESCE(SUM(input_total + output_total), 0)
         FROM usage_event
         WHERE occurred_at >= ?1 AND occurred_at < ?2 AND {}",
        group_filter(group)
    );
    connection
        .query_row(&sql, rusqlite::params![start_ms, end_ms], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(|error| error.to_string())
}

fn group_first_event(connection: &Connection, group: &str) -> Result<Option<i64>, String> {
    let sql = format!(
        "SELECT MIN(occurred_at) FROM usage_event WHERE {}",
        group_filter(group)
    );
    connection
        .query_row(&sql, [], |row| row.get(0))
        .map_err(|error| error.to_string())
}

fn first_event_all(connection: &Connection) -> Result<Option<i64>, String> {
    connection
        .query_row("SELECT MIN(occurred_at) FROM usage_event", [], |row| {
            row.get(0)
        })
        .map_err(|error| error.to_string())
}

fn week_point(cycle: &GroupCycle, account: Option<&str>) -> WeekPoint {
    let percent = account.and_then(|account| cycle.percent_of(account));
    WeekPoint {
        start: local_date(cycle.start_ms),
        end: local_date(cycle.effective_end_ms),
        resets_at_ms: cycle.end_ms,
        used_percent: percent,
        used_tokens: cycle
            .max_tokens
            .zip(percent)
            .map(|(max, percent)| (max as f64 * percent / 100.0).round() as i64),
        used_cost_usd: cycle.max_cost_usd.zip(percent).map(|(max, percent)| max * percent / 100.0),
        max_tokens: cycle.max_tokens,
        max_cost_usd: cycle.max_cost_usd,
        sample_at: local_stamp(cycle.sample_ms),
        open: cycle.open,
        missing: cycle.missing,
    }
}

fn local_date(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|time| time.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

fn local_stamp(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|time| time.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}
