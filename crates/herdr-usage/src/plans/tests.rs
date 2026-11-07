//! 推算口径的单测：全部用内存用量库 + 手写比例样本，不依赖本机数据。
//! Grok 账单日志是文件来源，用临时目录里的一个 `unified.jsonl` 夹具。

use super::*;
use crate::db;
use chrono::TimeZone;
use rusqlite::Connection;
use std::fs;

/// 本地时间 → epoch 毫秒。用 `Local` 是为了让断言里的 `YYYY-MM-DD` 与页面一致。
fn at(year: i32, month: u32, day: u32, hour: u32) -> i64 {
    Local
        .with_ymd_and_hms(year, month, day, hour, 0, 0)
        .single()
        .expect("local time")
        .timestamp_millis()
}

fn usage_db() -> Connection {
    let connection = Connection::open_in_memory().expect("memory db");
    connection.execute_batch(db::SCHEMA).expect("schema");
    connection
}

/// 一条用量事件。token 数放在 `input_total`，因此 `input_total + output_total`
/// 就是该事件的 Token 用量。
fn event(
    connection: &Connection,
    id: &str,
    source: &str,
    provider: Option<&str>,
    occurred_at: i64,
    cost_usd: Option<f64>,
    tokens: i64,
) {
    connection
        .execute(
            "INSERT INTO usage_event(source, event_id, model, model_source, provider,
                 input_total, cache_read, cache_write, output_total, reasoning, cost_usd, occurred_at)
             VALUES (?1, ?2, 'model', 'event', ?3, ?4, 0, 0, 0, 0, ?5, ?6)",
            rusqlite::params![source, id, provider, tokens, cost_usd, occurred_at],
        )
        .expect("insert event");
}

fn sample(account: &str, recorded_at_ms: i64, used_percent: f64, resets_at_ms: i64) -> Sample {
    // 与账本读取一致：有 email 用 email，否则用 `secret:` 之后的哈希。
    let email = account
        .split_once("email:")
        .and_then(|(_, rest)| rest.split('|').next())
        .map(str::to_owned);
    let label = email.unwrap_or_else(|| {
        account
            .rsplit_once(':')
            .map(|(_, tail)| tail)
            .unwrap_or(account)
            .to_owned()
    });
    Sample {
        account_key: account.to_owned(),
        label,
        recorded_at_ms,
        used_percent,
        resets_at_ms,
        window_days: WINDOW_DAYS,
    }
}

/// 账本里的账户标识。
fn oauth(email: &str) -> String {
    format!("oauth|email:{email}|project:proj")
}

fn samples_of(pairs: Vec<(&'static str, Vec<Sample>)>) -> BTreeMap<&'static str, Vec<Sample>> {
    pairs.into_iter().collect()
}

fn options() -> PlanOptions {
    PlanOptions
}

fn plan<'a>(report: &'a PlanReport, id: &str) -> &'a PlanEntry {
    report
        .plans
        .iter()
        .find(|entry| entry.id == id)
        .expect("plan present")
}

fn build_now(
    connection: &Connection,
    samples: &BTreeMap<&'static str, Vec<Sample>>,
    options: &PlanOptions,
    now: i64,
) -> PlanReport {
    build(connection, samples, options, now, Vec::new()).expect("report")
}

// ---------------------------------------------------------------------------
// 推算
// ---------------------------------------------------------------------------

#[test]
fn a_single_account_period_derives_the_cap_from_usage_over_the_used_fraction() {
    let connection = usage_db();
    let end = at(2026, 10, 8, 0);
    let start = end - WINDOW_DAYS * DAY_MS;
    let now = at(2026, 10, 5, 0);
    event(
        &connection,
        "1",
        "codex",
        None,
        start + 3_600_000,
        Some(6.0),
        600,
    );
    event(
        &connection,
        "2",
        "codex",
        None,
        start + 2 * DAY_MS,
        Some(4.0),
        400,
    );
    // 别的 provider 的行绝不能计入 Codex 组。
    event(
        &connection,
        "3",
        "omp",
        Some("codebuddy"),
        start + 4 * DAY_MS,
        Some(1000.0),
        100_000,
    );

    let samples = samples_of(vec![(
        "codex",
        vec![sample("acct", end - 3 * DAY_MS, 25.0, end)],
    )]);
    let report = build_now(&connection, &samples, &options(), now);
    let codex = plan(&report, "codex");

    assert!(codex.account_resolved);
    assert_eq!(codex.account.as_deref(), Some("acct"));
    assert_eq!(codex.used_percent, Some(25.0));
    assert_eq!(codex.group_percent, Some(25.0));
    // U = $10 / 1000 token，Σf = 0.25 → 上限 $40 / 4000 token。
    assert_eq!(codex.max_cost_usd, Some(40.0));
    assert_eq!(codex.max_tokens, Some(4000));
    assert_eq!(codex.used_cost_usd, Some(10.0));
    assert_eq!(codex.used_tokens, Some(1000));
    assert!(codex.open);
    assert_eq!(codex.weekly.len(), 1);
    let point = &codex.weekly[0];
    assert_eq!(point.start, "2026-10-01");
    assert_eq!(point.end, "2026-10-05", "进行中的周期统计到当前时刻");
    assert_eq!(point.resets_at_ms, end);
    assert!(point.missing < MAX_MISSING_RATIO);
    let period = codex.period.as_ref().expect("period");
    assert_eq!(period.start, "2026-10-01");
    assert_eq!(period.end, "2026-10-08");
    assert_eq!(period.days, WINDOW_DAYS);
}

#[test]
fn the_codex_group_counts_provider_labelled_rows_and_skips_other_providers() {
    let connection = usage_db();
    let end = at(2026, 10, 8, 0);
    let start = end - WINDOW_DAYS * DAY_MS;
    let now = at(2026, 10, 5, 0);
    event(
        &connection,
        "1",
        "omp",
        Some("openai-codex"),
        start + 3_600_000,
        Some(8.0),
        800,
    );
    event(
        &connection,
        "2",
        "omp",
        Some("opencode-go"),
        start + 7_200_000,
        Some(90.0),
        9000,
    );
    let samples = samples_of(vec![(
        "codex",
        vec![sample("acct", end - DAY_MS, 40.0, end)],
    )]);
    let report = build_now(&connection, &samples, &options(), now);
    let codex = plan(&report, "codex");
    assert_eq!(codex.max_cost_usd, Some(20.0), "只统计 openai-codex 的 $8");
    assert_eq!(codex.max_tokens, Some(2000));
}

// ---------------------------------------------------------------------------
// 周期切分
// ---------------------------------------------------------------------------

#[test]
fn a_period_superseded_by_the_next_one_stops_counting_where_the_next_begins() {
    let connection = usage_db();
    let end_a = at(2026, 10, 8, 0); // 起点 10-01
    let end_b = at(2026, 10, 10, 0); // 起点 10-03，与 A 重叠
    let now = at(2026, 10, 5, 0);
    event(
        &connection,
        "a",
        "codex",
        None,
        at(2026, 10, 1, 1),
        Some(4.0),
        400,
    );
    event(
        &connection,
        "b",
        "codex",
        None,
        at(2026, 10, 4, 12),
        Some(1.0),
        100,
    );
    let samples = samples_of(vec![(
        "codex",
        vec![
            sample("acct", at(2026, 10, 2, 0), 40.0, end_a),
            sample("acct", at(2026, 10, 4, 0), 10.0, end_b),
        ],
    )]);
    let report = build_now(&connection, &samples, &options(), now);
    let codex = plan(&report, "codex");
    assert_eq!(codex.weekly.len(), 2);

    let first = &codex.weekly[0];
    assert_eq!(first.start, "2026-10-01");
    assert_eq!(first.end, "2026-10-03", "被下一周期的起点截断");
    assert_eq!(first.resets_at_ms, end_a);
    assert!(!first.open);
    // 只有 [10-01, 10-03) 的用量：$4 / 400 token ÷ 0.4。
    assert_eq!(first.max_cost_usd, Some(10.0));
    assert_eq!(first.max_tokens, Some(1000));

    let second = &codex.weekly[1];
    assert_eq!(second.start, "2026-10-03");
    assert_eq!(second.end, "2026-10-05");
    assert!(second.open);
    // 只有 [10-03, 10-05) 的用量：$1 / 100 token ÷ 0.1 —— 与上一轮同量级。
    assert_eq!(second.max_cost_usd, Some(10.0));
    assert_eq!(second.max_tokens, Some(1000));

    // 「当前周期」取起点最晚者。
    assert_eq!(codex.period.as_ref().expect("period").resets_at_ms, end_b);
    assert_eq!(codex.used_percent, Some(10.0));
    assert!(codex.sample_at.is_some());
}

#[test]
fn a_fraction_drop_inside_one_period_counts_as_extra_granted_quota() {
    let connection = usage_db();
    let end = at(2026, 10, 8, 0);
    let start = end - WINDOW_DAYS * DAY_MS;
    let now = at(2026, 10, 5, 0);
    event(
        &connection,
        "1",
        "codex",
        None,
        start + 3_600_000,
        Some(130.0),
        13_000,
    );
    let samples = samples_of(vec![(
        "codex",
        vec![
            sample("acct", at(2026, 10, 1, 2), 90.0, end),
            sample("acct", at(2026, 10, 2, 2), 5.0, end),
            sample("acct", at(2026, 10, 3, 2), 40.0, end),
        ],
    )]);
    let report = build_now(&connection, &samples, &options(), now);
    let codex = plan(&report, "codex");
    // 回落前的峰值也要计入：0.9 + 0.4 = 1.3，且仍算同一个周期。
    assert_eq!(codex.weekly.len(), 1);
    assert_eq!(codex.group_percent, Some(130.0));
    assert_eq!(codex.used_percent, Some(40.0));
    assert_eq!(codex.max_cost_usd, Some(100.0));
    assert_eq!(codex.max_tokens, Some(10_000));
    assert_eq!(codex.used_cost_usd, Some(40.0));
    assert_eq!(codex.used_tokens, Some(4000));
}

#[test]
fn millisecond_jitter_keeps_one_period_and_a_real_change_starts_a_new_one() {
    let now = at(2026, 10, 5, 0);
    let end = at(2026, 10, 8, 0);
    let samples = vec![
        sample("acct", at(2026, 10, 1, 1), 10.0, end),
        sample("acct", at(2026, 10, 1, 2), 12.0, end + 30_000),
        sample(
            "acct",
            at(2026, 10, 1, 3),
            20.0,
            end + 2 * CYCLE_TOLERANCE_MS,
        ),
    ];
    let cycles = account_cycles(&samples, now);
    let cycles = &cycles["acct"];
    assert_eq!(cycles.len(), 2, "30 秒抖动归为一个周期");
    assert_eq!(cycles[0].end_ms, end + 30_000);
    assert_eq!(cycles[0].sum_percent, 12.0);
    assert_eq!(cycles[1].sum_percent, 20.0);
    // 新周期起点早于旧周期名义终点时，旧周期在起点处截断。
    assert_eq!(cycles[0].effective_end_ms, cycles[1].start_ms);
}

#[test]
fn a_period_whose_start_predates_the_usage_database_is_dropped_but_the_open_one_is_kept() {
    let connection = usage_db();
    // 用量库从 10-02 12:00 才开始。
    event(
        &connection,
        "1",
        "codex",
        None,
        at(2026, 10, 2, 12),
        Some(1.0),
        100,
    );
    event(
        &connection,
        "2",
        "codex",
        None,
        at(2026, 10, 4, 12),
        Some(2.0),
        200,
    );
    let now = at(2026, 10, 5, 0);
    let samples = samples_of(vec![(
        "codex",
        vec![
            // 旧周期：起点 09-26，用量库缺了 6.5 天。
            sample("acct", at(2026, 10, 1, 0), 50.0, at(2026, 10, 3, 0)),
            // 进行中的周期：起点 10-03，完全被覆盖。
            sample("acct", at(2026, 10, 4, 0), 20.0, at(2026, 10, 10, 0)),
        ],
    )]);
    let report = build(&connection, &samples, &options(), now, Vec::new()).expect("report");
    let notes = &report.notes;
    let codex = plan(&report, "codex");
    assert_eq!(codex.weekly.len(), 1, "缺口过大的周期整条丢弃");
    let point = &codex.weekly[0];
    assert_eq!(point.start, "2026-10-03");
    assert!(point.open);
    assert_eq!(point.max_cost_usd, Some(10.0));
    assert_eq!(point.max_tokens, Some(1000));
    assert!(
        notes.iter().any(|note| note.contains("1 个历史周期")),
        "{notes:?}"
    );
}

#[test]
fn two_accounts_align_into_one_shared_period_and_the_first_new_period_truncates_it() {
    let end = at(2026, 10, 8, 0);
    let now = at(2026, 10, 5, 0);
    let samples = vec![
        sample(&oauth("a@example.com"), at(2026, 10, 2, 0), 30.0, end),
        // 5 分钟后开新周期：两份订阅仍然算同一个组周期。
        sample(
            &oauth("b@example.com"),
            at(2026, 10, 2, 0),
            50.0,
            end + 300_000,
        ),
        // A 提前开新周期，组周期必须在这里截断。
        sample(
            &oauth("a@example.com"),
            at(2026, 10, 4, 0),
            10.0,
            at(2026, 10, 10, 0),
        ),
    ];
    let groups = group_cycles(&samples, now);
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].members.len(), 2, "两个账户对齐到一个组周期");
    assert_eq!(groups[0].sum_percent, 80.0);
    assert_eq!(groups[0].percent_of(&oauth("a@example.com")), Some(30.0));
    assert_eq!(groups[0].percent_of(&oauth("b@example.com")), Some(50.0));
    assert_eq!(groups[1].members.len(), 1);
    // 旧周期在 A 的新周期起点截断。
    assert_eq!(
        groups[0].members[0]
            .1
            .effective_end_ms
            .min(groups[1].start_ms),
        groups[1].start_ms
    );
}

#[test]
fn one_account_never_merges_two_of_its_own_periods_into_one_group_period() {
    // Codex 的相邻周期可以只差几分钟，绝不能被对齐容差并成一个。
    let end = at(2026, 10, 8, 0);
    let now = at(2026, 10, 5, 0);
    let samples = vec![
        sample("acct", at(2026, 10, 1, 0), 30.0, end),
        sample("acct", at(2026, 10, 1, 1), 40.0, end + 3_600_000),
    ];
    assert_eq!(group_cycles(&samples, now).len(), 2);
}

// ---------------------------------------------------------------------------
// 账户归属
// ---------------------------------------------------------------------------

#[test]
fn the_ledger_infos_and_mappings_decide_every_account() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ledger = dir.path().join("agent.db");
    let end = at(2026, 10, 8, 0);
    write_ledger(
        &ledger,
        &[(
            "openai-codex",
            "openai-codex:primary",
            "oauth|account:1|email:carter.gogo@gmail.com|org:1",
            Some("carter.gogo@gmail.com"),
            0.39,
            end,
            end - 60_000,
        )],
    );

    let paths = LedgerPaths {
        omp_dbs: vec![ledger],
        grok_log: dir.path().join("missing.jsonl"),
    };
    let (samples, ledger_found, _) = read_samples(&paths);
    assert!(ledger_found);
    assert_eq!(samples["codex"].len(), 1);
    // 归一化：比例 → 百分比，周期长度固定 7 天。
    assert!((samples["codex"][0].used_percent - 39.0).abs() < 1e-9);
    assert_eq!(samples["codex"][0].window_days, WINDOW_DAYS);

    // 单账户组 resolve 直接认领该账户。
    let infos = account_infos(&samples["codex"]);
    let assigned = resolve("codex", &infos);
    assert_eq!(
        assigned["codex"].as_deref(),
        Some("oauth|account:1|email:carter.gogo@gmail.com|org:1")
    );
}

// ---------------------------------------------------------------------------
// Grok 账单日志
// ---------------------------------------------------------------------------

const GROK_PERIOD_START: &str = "2026-09-13T14:20:07.958146+00:00";
const GROK_PERIOD_END: &str = "2026-09-20T14:20:07.958146+00:00";

fn grok_log(dir: &Path) -> PathBuf {
    let log = dir.join("unified.jsonl");
    let line = |ts: &str, percent: Option<f64>| {
        let percent = match percent {
            Some(percent) => format!(r#""creditUsagePercent":{percent},"#),
            None => String::new(),
        };
        format!(
            r#"{{"ts":"{ts}","msg":"billing: fetched credits config","ctx":{{"subscriptionTier":"SuperGrok","config":{{{percent}"currentPeriod":{{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"{GROK_PERIOD_START}","end":"{GROK_PERIOD_END}"}}}}}}}}"#
        )
    };
    let lines = [
        line("2026-09-14T14:35:34.531Z", Some(3.0)),
        line("2026-09-15T04:08:20.264Z", Some(7.0)),
        // 缺 creditUsagePercent 的行必须跳过（实测 100 条里有 12 条）。
        line("2026-09-15T04:08:21.275Z", None),
        r#"{"ts":"2026-09-15T04:08:22.000Z","msg":"something else","ctx":{}}"#.to_owned(),
    ];
    fs::write(&log, lines.join("\n") + "\n").expect("write grok log");
    log
}

#[test]
fn the_grok_log_supplies_the_fraction_and_the_period_bounds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = LedgerPaths {
        omp_dbs: vec![dir.path().join("missing-agent.db")],
        grok_log: grok_log(dir.path()),
    };
    let (samples, ledger_found, notes) = read_samples(&paths);
    assert!(ledger_found, "Grok 日志本身就是一处比例来源");
    assert!(
        notes.iter().any(|note| note.contains("未找到额度账本")),
        "{notes:?}"
    );
    assert!(samples["codex"].is_empty());

    let grok = &samples["grok"];
    assert_eq!(grok.len(), 2, "缺比例的整行跳过");
    assert_eq!(grok[0].used_percent, 3.0);
    assert_eq!(grok[1].used_percent, 7.0);
    assert_eq!(grok[0].account_key, "grok");
    assert_eq!(grok[0].label, "SuperGrok");
    assert_eq!(grok[0].window_days, WINDOW_DAYS);
    assert_eq!(
        grok[0].resets_at_ms,
        parse_rfc3339_ms(GROK_PERIOD_END).expect("end")
    );
    assert_eq!(
        grok[1].recorded_at_ms,
        parse_rfc3339_ms("2026-09-15T04:08:20.264Z").expect("ts")
    );
}

#[test]
fn the_grok_plan_derives_its_cap_from_the_logged_period() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = LedgerPaths {
        omp_dbs: vec![dir.path().join("missing-agent.db")],
        grok_log: grok_log(dir.path()),
    };
    let connection = usage_db();
    // 本周期实测 $9.22（与 verification 里的手算一致）。
    event(
        &connection,
        "1",
        "grok",
        None,
        at(2026, 9, 14, 1),
        Some(9.22),
        0,
    );
    let now = parse_rfc3339_ms("2026-09-15T05:00:00Z").expect("now");
    let report = assemble(&connection, &options(), &paths, now).expect("report");
    let grok = plan(&report, "grok");
    assert!(grok.account_resolved);
    assert_eq!(grok.account.as_deref(), Some("SuperGrok"));
    assert_eq!(grok.used_percent, Some(7.0));
    assert_eq!(grok.group_percent, Some(7.0));
    let max = grok.max_cost_usd.expect("cap");
    assert!((max - 9.22 / 0.07).abs() < 1e-6, "{max}");
    assert_eq!(grok.used_cost_usd, Some(max * 0.07));
    assert!(grok.open);
    let period = grok.period.as_ref().expect("period");
    assert_eq!(period.start, "2026-09-13");
    assert_eq!(period.end, "2026-09-20");
    assert_eq!(period.days, WINDOW_DAYS);
}

#[test]
fn missing_ledgers_degrade_to_empty_samples_without_panicking() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = LedgerPaths {
        omp_dbs: vec![dir.path().join("agent.db")],
        grok_log: dir.path().join("unified.jsonl"),
    };
    let (samples, ledger_found, notes) = read_samples(&paths);
    assert!(!ledger_found);
    assert!(samples.values().all(Vec::is_empty));
    assert_eq!(notes.len(), 2, "{notes:?}");

    // 文件存在但没有 `usage_history`（omp 改了表）时同样跳过。
    let empty = dir.path().join("empty.db");
    Connection::open(&empty)
        .expect("open")
        .execute("CREATE TABLE other(x)", [])
        .expect("create");
    let paths = LedgerPaths {
        omp_dbs: vec![empty],
        grok_log: dir.path().join("unified.jsonl"),
    };
    let (samples, ledger_found, _) = read_samples(&paths);
    assert!(!ledger_found);
    assert!(samples["codex"].is_empty());

    // 完全没有样本时报告仍然完整：字段为空、备注解释原因。
    let connection = usage_db();
    let report = build(
        &connection,
        &samples,
        &options(),
        at(2026, 10, 5, 0),
        Vec::new(),
    )
    .expect("report");
    let notes = &report.notes;
    let codex = plan(&report, "codex");
    assert_eq!(codex.used_percent, None);
    assert_eq!(codex.max_cost_usd, None);
    assert!(codex.period.is_none());
    assert!(codex.weekly.is_empty());
    assert!(!report.ledger_found);
    assert!(
        notes.iter().any(|note| note.contains("没有额度比例样本")),
        "{notes:?}"
    );
}

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

/// 一份与 omp 一致的 `usage_history`（12 列）及其若干行。
fn write_ledger(path: &Path, rows: &[(&str, &str, &str, Option<&str>, f64, i64, i64)]) {
    // (provider, limit_id, account_key, email, used_fraction, resets_at, recorded_at)
    let connection = Connection::open(path).expect("open ledger");
    connection
        .execute_batch(
            "CREATE TABLE usage_history(
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 recorded_at INTEGER NOT NULL,
                 provider TEXT NOT NULL,
                 account_key TEXT NOT NULL,
                 email TEXT,
                 account_id TEXT,
                 limit_id TEXT NOT NULL,
                 label TEXT NOT NULL,
                 window_label TEXT,
                 used_fraction REAL,
                 status TEXT,
                 resets_at INTEGER)",
        )
        .expect("ledger schema");
    for (provider, limit_id, account_key, email, fraction, resets_at, recorded_at) in rows {
        connection
            .execute(
                "INSERT INTO usage_history(recorded_at, provider, account_key, email, limit_id,
                     label, window_label, used_fraction, status, resets_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'Weekly', 'Weekly', ?6, 'ok', ?7)",
                rusqlite::params![
                    recorded_at,
                    provider,
                    account_key,
                    email,
                    limit_id,
                    fraction,
                    resets_at
                ],
            )
            .expect("insert ledger row");
    }
}
