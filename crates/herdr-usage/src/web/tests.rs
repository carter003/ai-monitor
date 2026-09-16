use super::*;
use chrono::TimeZone;

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
