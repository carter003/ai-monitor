use super::*;
use crate::plans::PlanOptions;
use chrono::TimeZone;
use std::io::{Read, Write};
use std::net::TcpStream;

fn fixture() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../../migrations/schema.sql"))
        .unwrap();
    db.execute_batch(
        "INSERT INTO model_alias VALUES ('raw-a', 'vendor/a', 0, 'manual', NULL);
        INSERT INTO model_alias VALUES ('free', NULL, 1, 'ignore', NULL);",
    )
    .unwrap();
    let midnight = Local
        .with_ymd_and_hms(2026, 9, 12, 0, 0, 0)
        .earliest()
        .unwrap()
        .timestamp_millis();
    for (id, model, at, cost) in [
        ("1", Some("raw-a"), midnight, Some(0.25)),
        ("2", Some("vendor/a"), midnight + 1, Some(0.5)),
        ("3", Some("free"), midnight + 2, None),
        ("4", None, midnight + 3, None),
        ("5", Some("raw-a"), midnight - 1, Some(1.0)),
        ("6", Some("vendor/a"), midnight + 86400000, Some(2.0)),
    ] {
        db.execute(
            "INSERT INTO usage_event(source, event_id, model, model_source, input_total, cache_read, cache_write, output_total, reasoning, cost_usd, occurred_at)
             VALUES ('codex',?1,?2,'event',100,60,0,20,10,?3,?4)",
            rusqlite::params![id, model, cost, at],
        )
        .unwrap();
    }
    db
}
fn get(db: &Connection, query: &str) -> Report {
    report(
        db,
        Query::parse(query).unwrap(),
        NaiveDate::from_ymd_opt(2026, 9, 12).unwrap(),
    )
    .unwrap()
}
#[test]
fn daily_boundaries_aliases_and_accounting_reconcile() {
    let db = fixture();
    let r = get(&db, "start=2026-09-10&end=2026-09-12");
    assert_eq!(r.daily.len(), 3);
    assert_eq!(r.daily[0].total.events, 0);
    assert_eq!(r.daily[1].total.tokens, 120);
    assert_eq!(r.daily[2].total.tokens, 480);
    assert_eq!(r.total.tokens, 600); // cache/reasoning never double counted
    assert_eq!(r.total.cost_usd, Some(1.75));
    assert_eq!(r.total.unpriced, 2);
    assert_eq!(
        r.ranking.iter().map(|r| r.total.tokens).sum::<i64>(),
        r.total.tokens
    );
    assert_eq!(
        r.details.iter().map(|r| r.total.tokens).sum::<i64>(),
        r.total.tokens
    );
    assert_eq!(r.ranking[0].model.as_deref(), Some("vendor/a"));
    assert_eq!(r.ranking[0].total.tokens, 360);
    assert_eq!(r.overview["today"].tokens, 480);
    assert_eq!(r.overview["week"].tokens, 600);
    assert_eq!(r.overview["all"].tokens, 720);
}
#[test]
fn single_model_history_and_unknown_are_filterable() {
    let db = fixture();
    let r = get(&db, "start=2026-09-11&end=2026-09-12&model=vendor%2Fa");
    assert_eq!(r.total.tokens, 360);
    assert_eq!(r.daily[1].total.cost_usd, Some(0.75));
    assert_eq!(r.overview["today"].tokens, 480); // overview remains global
    let r = get(&db, "unknown=1");
    assert_eq!(r.total.tokens, 120);
    assert_eq!(r.total.cost_usd, None);
    assert_eq!(r.total.unpriced, 1);
    let r = get(&db, "model=free");
    assert_eq!(r.total.tokens, 120); // ignored for pricing, retained for reconciliation
    let r = get(&db, "model=%27%20OR%201%3D1--");
    assert_eq!(r.total.events, 0);
}
#[test]
fn empty_database_and_invalid_queries() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../../migrations/schema.sql"))
        .unwrap();
    let r = get(&db, "");
    assert_eq!(r.daily.len(), 30);
    assert_eq!(r.total.tokens, 0);
    assert!(r.models.is_empty());
    assert!(r.first_day.is_none());
    for q in [
        "start=2026-02-30",
        "start=2026-1-1",
        "model=%QQ",
        "model=%FF",
        "bad=1",
    ] {
        assert!(Query::parse(q).is_err(), "{q}");
    }
    for q in [
        "start=2026-09-13&end=2026-09-12",
        "start=2000-01-01&end=2026-09-12",
    ] {
        assert!(report(&db, Query::parse(q).unwrap(), Local::now().date_naive()).is_err());
    }
}
#[test]
fn missing_database_is_not_created() {
    let path = std::env::temp_dir().join(format!("usage-web-missing-{}.db", std::process::id()));
    assert!(load(&path, Query::default()).is_err());
    assert!(!path.exists());
}

#[test]
fn global_cache_refreshes_after_writer_commit_and_range_uses_time_index() {
    let path = std::env::temp_dir().join(format!(
        "usage-web-cache-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let writer = crate::db::open(&path).unwrap();
    let at = Local::now().timestamp_millis();
    writer.execute("INSERT INTO usage_event(source,event_id,model,input_total,cache_read,cache_write,output_total,reasoning,cost_usd,occurred_at)
        VALUES('codex','one','m',10,0,0,2,0,0.1,?1)", [at]).unwrap();
    assert_eq!(
        load(&path, Query::default()).unwrap().overview["all"].events,
        1
    );
    writer.execute("INSERT INTO usage_event(source,event_id,model,input_total,cache_read,cache_write,output_total,reasoning,cost_usd,occurred_at)
        VALUES('codex','two','m',10,0,0,2,0,0.1,?1)", [at]).unwrap();
    let report = load(&path, Query::default()).unwrap();
    assert_eq!(report.overview["all"].events, 2);
    assert_eq!(report.total.events, 2);
    let plan: String = writer
        .query_row(
            "EXPLAIN QUERY PLAN SELECT date(e.occurred_at / 1000, 'unixepoch', 'localtime')
         FROM usage_event e LEFT JOIN model_alias a ON a.raw_model = e.model
         WHERE e.occurred_at >= ?1 AND e.occurred_at < ?2
         GROUP BY 1",
            rusqlite::params![at - 1, at + 1],
            |row| row.get(3),
        )
        .unwrap();
    assert!(plan.contains("idx_usage_time"), "{plan}");
}

#[test]
fn test_hourly_report_loads_and_reconciles() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("usage.db");
    let mut db = crate::db::open(&db_path).unwrap();

    let dt = Local.with_ymd_and_hms(2026, 9, 12, 14, 30, 0).earliest().unwrap();
    let at = dt.timestamp_millis();

    db.execute(
        "INSERT INTO usage_event(source, event_id, model, model_source, input_total, cache_read, cache_write, output_total, reasoning, cost_usd, occurred_at)
         VALUES ('codex', 'ev-1', 'openai/gpt-4o', 'event', 1000, 200, 0, 500, 0, 0.05, ?1)",
        rusqlite::params![at],
    ).unwrap();

    let later = at + 2 * 3600 * 1000;
    crate::db::rollup_closed_hours(&mut db, later).unwrap();
    drop(db);

    let report = super::hourly::load(&db_path, Some("2026-09")).unwrap();
    assert_eq!(report.month, "2026-09");
    assert!(report.available_months.contains(&"2026-09".to_string()));
    assert_eq!(report.summary.total_tokens, 1500);
    assert_eq!(report.summary.total_cost, Some(0.05));
    assert_eq!(report.summary.active_days, 1);
    assert_eq!(report.summary.peak_day, "2026-09-12");
    assert_eq!(report.summary.peak_hour, 14);
    assert_eq!(report.summary.peak_tokens, 1500);

    let day_row = report.days.iter().find(|d| d.day == "2026-09-12").expect("day row");
    assert_eq!(day_row.weekday, "周六");
    assert_eq!(day_row.total_tokens, 1500);
    assert_eq!(day_row.total_cost, Some(0.05));
    assert_eq!(day_row.hours.len(), 24);

    let hour_14 = &day_row.hours[14];
    assert_eq!(hour_14.hour, 14);
    assert_eq!(hour_14.input, 1000);
    assert_eq!(hour_14.output, 500);
    assert_eq!(hour_14.tokens, 1500);
    assert_eq!(hour_14.cache_read, 200);
    assert_eq!(hour_14.cost_usd, Some(0.05));
    assert_eq!(hour_14.events, 1);
    assert!(hour_14.closed);

    let hour_13 = &day_row.hours[13];
    assert_eq!(hour_13.tokens, 0);
    assert_eq!(hour_13.events, 0);
    assert_eq!(hour_13.cost_usd, None);

    assert!(super::hourly::load(&db_path, Some("2026-9")).is_err());
    assert!(super::hourly::load(&db_path, Some("invalid")).is_err());
}

#[test]
fn test_cloudflare_routes_and_navigation() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_path_buf();
    let _conn = crate::db::open(&db_path).unwrap();

    let server = server::Server::start(
        db_path.clone(),
        0,
        PlanOptions::default(),
        crate::cloudflare::CloudflareConfig::default(),
    )
    .unwrap();
    let addr = server.address();

    // 1. GET /cloudflare
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .write_all(b"GET /cloudflare HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut resp = String::new();
    stream.read_to_string(&mut resp).unwrap();
    assert!(resp.starts_with("HTTP/1.1 200 OK"));
    assert!(resp.contains("Cloudflare 资源用量与额度看板"));
    assert!(resp.contains("Workers Paid Plan"));

    // 2. GET /api/cloudflare (unconfigured)
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .write_all(b"GET /api/cloudflare HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut resp = String::new();
    stream.read_to_string(&mut resp).unwrap();
    assert!(resp.starts_with("HTTP/1.1 200 OK"));
    assert!(resp.contains(r#""configured":false"#));

    // 3. Navigation links in all HTML files
    for path in ["/", "/hourly", "/plans", "/requests", "/query", "/cloudflare"] {
        let mut stream = TcpStream::connect(addr).unwrap();
        write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 200 OK"), "Failed on path {path}");
        assert!(
            resp.contains(r#"href="/cloudflare""#),
            "Path {path} missing cloudflare nav link"
        );
    }
}

#[test]
fn test_cloudflare_api_with_cached_data() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let db_path = tmp.path().to_path_buf();
    let conn = crate::db::open(&db_path).unwrap();

    let account_id = "test_account_123";
    let now_ms = chrono::Utc::now().timestamp_millis();

    let summary = crate::cloudflare::build_current_period_summary(
        &crate::cloudflare::CloudflareQuotas::for_plan("paid"),
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
        None,
        0,
        0,
        None,
        12,
    );
    let ascii_card = crate::cloudflare::generate_ascii_card(&summary);
    let cached_report = crate::cloudflare::CloudflareReport {
        configured: true,
        plan: "paid".into(),
        billing_cycle: crate::cloudflare::BillingCycleInfo {
            start: "2026-09-08T00:00:00Z".into(),
            end: "2026-10-08T00:00:00Z".into(),
            start_date: "2026-09-08".into(),
            end_date: "2026-10-08".into(),
            total_days: 30,
            elapsed_days: 12,
            days_remaining: 18,
            elapsed_percent: 40.0,
            cycle_type: "套餐订阅周期".into(),
            is_fallback: false,
        },
        current_period: summary,
        ascii_card,
        daily_trend: vec![],
        monthly_history: vec![],
        stale: false,
        error: None,
        synced_at: now_ms,
    };
    let json = serde_json::to_string(&cached_report).unwrap();
    conn.execute(
        "INSERT INTO cf_sync_state(account_id, period_start, period_end, last_synced_at, cached_summary_json)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![account_id, "2026-09-08T00:00:00Z", "2026-10-08T00:00:00Z", now_ms, json],
    ).unwrap();

    let cf_config = crate::cloudflare::CloudflareConfig {
        account_id: Some(account_id.into()),
        api_token: Some("dummy_token".into()),
        plan: "paid".into(),
        billing_day: Some(8),
    };
    let server = server::Server::start(
        db_path,
        0,
        PlanOptions::default(),
        cf_config,
    ).unwrap();
    let addr = server.address();

    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .write_all(b"GET /api/cloudflare HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut resp = String::new();
    stream.read_to_string(&mut resp).unwrap();

    assert!(resp.starts_with("HTTP/1.1 200 OK"));
    assert!(resp.contains(r#""configured":true"#));
    assert!(resp.contains("3.24M"));
    assert!(resp.contains("32.4%"));
    assert!(resp.contains("Deployments"));
    assert!(resp.contains("workers_scripts"));
    assert!(resp.contains("workers_build_minutes_limit"));
    assert!(resp.contains("WORKERS"));
}
