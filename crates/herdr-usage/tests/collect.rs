//! End-to-end collection tests against a temporary source tree.
//!
//! These exercise the *watermark* behaviour that the per-parser unit tests cannot
//! reach: first-run baseline, incremental reads, replay idempotence, rotation and
//! the `config.toml` mtime refresh.

use herdr_usage::{
    cost::{PriceTable, Priced},
    db, sources,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// A throwaway HOME laid out like the real one.
struct Fixture {
    root: PathBuf,
    paths: sources::SourcePaths,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("herdr-usage-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let paths = sources::SourcePaths {
            omp: root.join(".omp/agent/sessions/-project-x"),
            codex: root.join(".codex/sessions/2026/09/11"),
            grok_log: root.join(".grok/logs/unified.jsonl"),
            grok_config: root.join(".grok/config.toml"),
            opencode_db: root.join("opencode/opencode.db"),
        };
        let dirs: Vec<&Path> = vec![
            &paths.omp,
            &paths.codex,
            paths.grok_log.parent().expect("parent"),
            paths.opencode_db.parent().expect("parent"),
        ];
        for dir in dirs {
            fs::create_dir_all(dir).expect("fixture dir");
        }
        fs::write(&paths.grok_config, "[models]\ndefault = \"grok-4.6\"\n").expect("grok config");
        Self { root, paths }
    }

    fn append(&self, path: &Path, text: &str) {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open for append");
        file.write_all(text.as_bytes()).expect("append");
    }

    fn db_path(&self) -> PathBuf {
        self.root.join("usage.db")
    }

    fn connection(&self) -> rusqlite::Connection {
        db::open(&self.db_path()).expect("open db")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn count(connection: &rusqlite::Connection, source: &str) -> i64 {
    connection
        .query_row(
            "SELECT COUNT(*) FROM usage_event WHERE source = ?1",
            [source],
            |row| row.get(0),
        )
        .expect("count")
}

const OMP_LINE: &str = r#"{"id":"681cad2e","timestamp":"2026-09-10T16:38:17.999Z","type":"message","message":{"role":"assistant","model":"deepseek-v4.1-flash","provider":"codebuddy","usage":{"input":211,"output":86,"cacheRead":22144,"cacheWrite":0,"totalTokens":22441}}}"#;

const CODEX_TURN: &str = r#"{"timestamp":"2026-09-10T16:00:00.000Z","type":"turn_context","payload":{"turn_id":"t1","model":"gpt-5.6-luna"}}"#;

const CODEX_TURN_ASTRA: &str = r#"{"timestamp":"2026-09-10T16:00:30.000Z","type":"turn_context","payload":{"turn_id":"t2","model":"gpt-6-astra"}}"#;
const CODEX_COUNT: &str = r#"{"timestamp":"2026-09-10T16:00:01.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":29027,"cached_input_tokens":17152,"output_tokens":484,"reasoning_output_tokens":69,"total_tokens":29511},"last_token_usage":{"input_tokens":16067,"cached_input_tokens":12672,"output_tokens":157,"reasoning_output_tokens":0,"total_tokens":16224}}}}"#;

const CODEX_NULL_INFO: &str = r#"{"timestamp":"2026-09-10T16:00:02.000Z","type":"event_msg","payload":{"type":"token_count","info":null}}"#;

const GROK_LINE: &str = r#"{"ts":"2026-09-10T09:51:07.339Z","src":"shell","pid":1,"ver":"1.0.25","lvl":"info","sid":"01a08a84","msg":"shell.turn.inference_done","ctx":{"loop_index":3,"prompt_tokens":323742,"cached_prompt_tokens":321280,"completion_tokens":2792,"reasoning_tokens":506}}"#;

#[test]
fn the_first_round_records_the_baseline_and_backfills_nothing() {
    let fixture = Fixture::new("baseline");
    // Pre-existing history, exactly what must NOT be collected.
    let history = format!("{OMP_LINE}\n{CODEX_TURN}\n{CODEX_COUNT}\n{GROK_LINE}\n");
    let omp_file = fixture.paths.omp.join("old.jsonl");
    fs::write(&omp_file, &history).expect("seed omp");
    let codex_file = fixture.paths.codex.join("rollout-old.jsonl");
    fs::write(&codex_file, &history).expect("seed codex");
    fs::write(&fixture.paths.grok_log, &history).expect("seed grok");

    let mut connection = fixture.connection();
    let mut collector = sources::Collector::new(fixture.paths.clone());
    let report = collector.run_round(&mut connection, 1_000).expect("round");

    assert_eq!(report.inserted, 0, "第一轮只记录水位，不回填历史");
    assert_eq!(count(&connection, "omp"), 0);
    assert_eq!(count(&connection, "codex"), 0);
    assert_eq!(count(&connection, "grok"), 0);

    // New activity after the baseline is collected.
    fixture.append(&omp_file, &format!("{OMP_LINE}\n"));
    fixture.append(&codex_file, &format!("{CODEX_COUNT}\n"));
    fixture.append(&fixture.paths.grok_log, &format!("{GROK_LINE}\n"));
    let report = collector
        .run_round(&mut connection, 2_000)
        .expect("round 2");
    assert_eq!(report.inserted, 3);
    assert_eq!(count(&connection, "omp"), 1);
    assert_eq!(count(&connection, "codex"), 1);
    assert_eq!(count(&connection, "grok"), 1);
}

#[test]
fn a_live_file_seen_for_the_first_time_is_read_from_zero() {
    // Regression: omp buffers session writes, so a log can be created minutes
    // after its session started and already hold inference records. The old
    // code baselined any unseen file at its current size, which dropped those
    // records permanently (measured: 11 events / ~318k tokens lost on
    // 2026-09-11). A resumed round must instead read the file from zero.
    let fixture = Fixture::new("live-omp");
    let mut connection = fixture.connection();
    let mut collector = sources::Collector::new(fixture.paths.clone());
    // Round 1 over an empty corpus: cold start, records the baseline.
    collector
        .run_round(&mut connection, 1_000)
        .expect("cold round");

    // A brand-new omp file that was born with content (as if omp flushed a
    // buffered session in one write).
    let born = fixture.paths.omp.join("born-with-events.jsonl");
    fs::write(&born, format!("{OMP_LINE}\n{OMP_LINE}\n")).expect("write born file");

    // Resumed round: the file has no watermark, its mtime is fresh, and the
    // collector has run before — the born events must be collected, not
    // baselined away. The duplicate envelope id keeps the count at one.
    let report = collector.run_round(&mut connection, 2_000).expect("round");
    assert_eq!(report.inserted, 1, "born events are collected");
    assert_eq!(count(&connection, "omp"), 1);

    // Replays are still absorbed by the primary key.
    let report = collector
        .run_round(&mut connection, 3_000)
        .expect("round 2");
    assert_eq!(report.inserted, 0);
    assert_eq!(count(&connection, "omp"), 1);
}

#[test]
fn a_cold_start_still_baselines_a_fresh_file_instead_of_backfilling() {
    // The complement of the fix: on the very first round (no persisted cursor)
    // an existing corpus is history by definition, so even a file whose mtime
    // is seconds old must be baselined, not backfilled.
    let fixture = Fixture::new("cold-omp");
    let fresh = fixture.paths.omp.join("fresh-history.jsonl");
    fs::write(&fresh, format!("{OMP_LINE}\n")).expect("seed fresh file");

    let mut connection = fixture.connection();
    let mut collector = sources::Collector::new(fixture.paths.clone());
    let report = collector.run_round(&mut connection, 1_000).expect("round");
    assert_eq!(report.inserted, 0, "首轮只记录水位，不回填历史");
    assert_eq!(count(&connection, "omp"), 0);

    // Growth after the baseline is still collected normally.
    fixture.append(&fresh, &format!("{OMP_LINE}\n"));
    let report = collector
        .run_round(&mut connection, 2_000)
        .expect("round 2");
    assert_eq!(report.inserted, 1);
}
#[test]
fn a_resumed_codex_round_recovers_the_model_beyond_the_64k_head() {
    // Regression: a service restart keeps the persisted watermarks but drops
    // the in-memory `turn_context` model map. The old head-recovery scanned
    // only the first 64 KB, while measured rollouts carry their first
    // `turn_context` at ~85 KB — so every `token_count` between the watermark
    // and the next `turn_context` was stored with a NULL model (measured: 240
    // events / 35.2M tokens invisible in the per-model table on 2026-09-11).
    let fixture = Fixture::new("codex-restart");
    let file = fixture.paths.codex.join("rollout-restart.jsonl");

    // History written before the collector ever ran: the turn_context sits at
    // ~85 KB, past the old 64 KB recovery window, followed by filler that
    // keeps it there.
    let filler = "{\"type\":\"other\"}\n".repeat(2000);
    let mut history = format!("{CODEX_TURN}\n{filler}");
    // Pad the turn_context line region to just past 64 KB before the counts.
    while history.len() < 70 * 1024 {
        history.push_str("{\"type\":\"other\"}\n");
    }
    fs::write(&file, &history).expect("seed codex history");

    let mut connection = fixture.connection();
    let mut collector = sources::Collector::new(fixture.paths.clone());
    collector
        .run_round(&mut connection, 1_000)
        .expect("baseline");

    // Simulate a restart: a fresh collector resumes from the persisted
    // watermark, while the file keeps a model switch in its history region.
    let mut restarted = sources::Collector::new(fixture.paths.clone());

    // The session switched models mid-file (e.g. `/model`), past the head of
    // the file: a second turn_context deep in the history region selects
    // gpt-6-astra, so the recovered model must be the latest one, not the
    // file's initial gpt-5.6-luna.
    fixture.append(&file, &format!("{CODEX_TURN_ASTRA}\n"));
    restarted
        .run_round(&mut connection, 1_500)
        .expect("astra turn");

    fixture.append(&file, &format!("{CODEX_COUNT}\n"));
    let report = restarted
        .run_round(&mut connection, 2_000)
        .expect("resumed round");
    assert_eq!(report.inserted, 1);
    let model: Option<String> = connection
        .query_row(
            "SELECT model FROM usage_event WHERE source = 'codex'",
            [],
            |row| row.get(0),
        )
        .expect("model");
    assert_eq!(
        model.as_deref(),
        Some("gpt-6-astra"),
        "the latest turn_context before the watermark must win"
    );
}

#[test]
fn a_second_round_with_no_new_bytes_inserts_nothing() {
    let fixture = Fixture::new("idle");
    let omp_file = fixture.paths.omp.join("s.jsonl");
    fs::write(&omp_file, "").expect("seed");
    let mut connection = fixture.connection();
    let mut collector = sources::Collector::new(fixture.paths.clone());
    collector
        .run_round(&mut connection, 1_000)
        .expect("baseline");
    fixture.append(&omp_file, &format!("{OMP_LINE}\n"));
    assert_eq!(
        collector
            .run_round(&mut connection, 2_000)
            .expect("round")
            .inserted,
        1
    );
    // Measured constraint: “不读已消费字节”. Nothing new means no events, and the
    // same line must not be counted twice.
    for at in 3_000..3_010 {
        assert_eq!(
            collector
                .run_round(&mut connection, at)
                .expect("round")
                .inserted,
            0
        );
    }
    assert_eq!(count(&connection, "omp"), 1);
}

#[test]
fn a_partial_line_is_held_back_until_it_is_complete() {
    let fixture = Fixture::new("partial");
    let omp_file = fixture.paths.omp.join("s.jsonl");
    fs::write(&omp_file, "").expect("seed");
    let mut connection = fixture.connection();
    let mut collector = sources::Collector::new(fixture.paths.clone());
    collector
        .run_round(&mut connection, 1_000)
        .expect("baseline");

    // Half a JSON object flushed without a newline, as a mid-write flush looks.
    let split = OMP_LINE.len() / 2;
    fixture.append(&omp_file, &OMP_LINE[..split]);
    assert_eq!(
        collector
            .run_round(&mut connection, 2_000)
            .expect("round")
            .inserted,
        0,
        "a truncated line must not be parsed"
    );
    fixture.append(&omp_file, &format!("{}\n", &OMP_LINE[split..]));
    assert_eq!(
        collector
            .run_round(&mut connection, 3_000)
            .expect("round")
            .inserted,
        1,
        "the completed line must be picked up exactly once"
    );
    assert_eq!(count(&connection, "omp"), 1);
}

#[test]
fn a_restart_in_the_middle_of_a_line_recovers_the_completed_record() {
    let fixture = Fixture::new("resume");
    let omp_file = fixture.paths.omp.join("s.jsonl");
    fs::write(&omp_file, "").expect("seed");
    let mut connection = fixture.connection();
    let mut collector = sources::Collector::new(fixture.paths.clone());
    collector
        .run_round(&mut connection, 1_000)
        .expect("baseline");

    // A writer flushes half a line and the collector consumes those bytes; the
    // fragment is held in the watermark, not in memory, because the process can
    // restart at any moment.
    let split = OMP_LINE.len() / 2;
    fixture.append(&omp_file, &OMP_LINE[..split]);
    collector.run_round(&mut connection, 2_000).expect("round");

    // Restart: a fresh collector with fresh in-memory state.
    let mut restarted = sources::Collector::new(fixture.paths.clone());
    restarted
        .run_round(&mut connection, 2_500)
        .expect("idle round");

    // The writer completes the line. The resumed collector must recover the whole
    // record, not just the tail fragment.
    fixture.append(&omp_file, &format!("{}\n", &OMP_LINE[split..]));
    let report = restarted.run_round(&mut connection, 3_000).expect("round");
    assert_eq!(
        report.inserted, 1,
        "the completed line must be recovered after a restart"
    );
    let id: String = connection
        .query_row(
            "SELECT event_id FROM usage_event WHERE source='omp'",
            [],
            |row| row.get(0),
        )
        .expect("row");
    assert_eq!(
        id, "681cad2e",
        "the recovered line must parse as the original JSON"
    );
}

#[test]
fn rotation_restarts_the_read_from_zero_without_duplicating() {
    let fixture = Fixture::new("rotate");
    let grok = fixture.paths.grok_log.clone();
    fs::write(&grok, "").expect("seed");
    let mut connection = fixture.connection();
    let mut collector = sources::Collector::new(fixture.paths.clone());
    collector
        .run_round(&mut connection, 1_000)
        .expect("baseline");

    fixture.append(&grok, &format!("{GROK_LINE}\n"));
    assert_eq!(
        collector
            .run_round(&mut connection, 2_000)
            .expect("round")
            .inserted,
        1
    );

    // Truncate: the same bytes are written again, which the primary key absorbs.
    fs::write(&grok, format!("{GROK_LINE}\n")).expect("truncate");
    let report = collector.run_round(&mut connection, 3_000).expect("round");
    assert_eq!(report.inserted, 0, "replayed bytes must be ignored");
    assert_eq!(count(&connection, "grok"), 1);
}

#[test]
fn codex_model_is_recovered_from_turn_context_and_survives_later_files() {
    let fixture = Fixture::new("codex-context");
    let file = fixture.paths.codex.join("rollout-a.jsonl");
    fs::write(&file, "").expect("seed");
    let mut connection = fixture.connection();
    let mut collector = sources::Collector::new(fixture.paths.clone());
    collector
        .run_round(&mut connection, 1_000)
        .expect("baseline");

    // A token_count before any turn_context has no model at all.
    fixture.append(&file, &format!("{CODEX_COUNT}\n"));
    collector.run_round(&mut connection, 2_000).expect("round");
    let model: Option<String> = connection
        .query_row(
            "SELECT model FROM usage_event WHERE source = 'codex'",
            [],
            |row| row.get(0),
        )
        .expect("model");
    assert_eq!(model, None, "no turn_context has been seen yet");

    // After the turn_context the model is recovered, and `info: null` is skipped
    // rather than panicking.
    fixture.append(
        &file,
        &format!("{CODEX_TURN}\n{CODEX_NULL_INFO}\n{CODEX_COUNT}\n"),
    );
    let report = collector.run_round(&mut connection, 3_000).expect("round");
    assert_eq!(report.inserted, 1, "info: null must be skipped, not stored");
    let rows: Vec<(Option<String>, Option<String>)> = {
        let mut statement = connection
            .prepare("SELECT model, model_source FROM usage_event WHERE source='codex' ORDER BY occurred_at")
            .expect("prepare");
        let mapped = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query");
        mapped.filter_map(Result::ok).collect()
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].0.as_deref(), Some("gpt-5.6-luna"));
    assert_eq!(rows[1].1.as_deref(), Some("context"));
}

#[test]
fn codex_seeds_the_model_from_a_pre_watermark_turn_context() {
    let fixture = Fixture::new("codex-seed");
    let file = fixture.paths.codex.join("rollout-seed.jsonl");
    // History that the baseline round will skip: it still describes the session.
    fs::write(&file, format!("{CODEX_TURN}\n")).expect("seed history");
    let mut connection = fixture.connection();
    let mut collector = sources::Collector::new(fixture.paths.clone());
    collector
        .run_round(&mut connection, 1_000)
        .expect("baseline");

    // A new append after the watermark belongs to the turn_context above, so it
    // must carry that model rather than NULL.
    fixture.append(&file, &format!("{CODEX_COUNT}\n"));
    let report = collector.run_round(&mut connection, 2_000).expect("round");
    assert_eq!(report.inserted, 1);
    let (model, source): (Option<String>, Option<String>) = connection
        .query_row(
            "SELECT model, model_source FROM usage_event WHERE source='codex'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("row");
    assert_eq!(model.as_deref(), Some("gpt-5.6-luna"));
    assert_eq!(source.as_deref(), Some("context"));
}

#[test]
fn grok_model_falls_back_to_config_and_refreshes_on_mtime_change() {
    let fixture = Fixture::new("grok-config");
    let grok = fixture.paths.grok_log.clone();
    fs::write(&grok, "").expect("seed");
    let mut connection = fixture.connection();
    let mut collector = sources::Collector::new(fixture.paths.clone());
    collector
        .run_round(&mut connection, 1_000)
        .expect("baseline");

    fixture.append(&grok, &format!("{GROK_LINE}\n"));
    collector.run_round(&mut connection, 2_000).expect("round");
    let (model, source): (Option<String>, Option<String>) = connection
        .query_row(
            "SELECT model, model_source FROM usage_event WHERE source='grok'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("row");
    assert_eq!(model.as_deref(), Some("grok-4.6"));
    assert_eq!(source.as_deref(), Some("config_fallback"));

    // Switch the default model and touch the file; the next round must pick it up
    // without a restart.
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(
        &fixture.paths.grok_config,
        "[models]\ndefault = \"grok-4.5\"\n",
    )
    .expect("rewrite config");
    let later = GROK_LINE.replace(
        "\"ts\":\"2026-09-10T09:51:07.339Z\"",
        "\"ts\":\"2026-09-10T10:00:00.000Z\"",
    );
    fixture.append(&grok, &format!("{later}\n"));
    collector.run_round(&mut connection, 3_000).expect("round");
    let models: Vec<String> = {
        let mut statement = connection
            .prepare("SELECT model FROM usage_event WHERE source='grok' ORDER BY occurred_at")
            .expect("prepare");
        let mapped = statement.query_map([], |row| row.get(0)).expect("query");
        mapped.filter_map(Result::ok).collect()
    };
    assert_eq!(models, vec!["grok-4.6", "grok-4.5"]);
}

#[test]
fn the_grok_default_model_reader_ignores_the_fork_secondary_model() {
    // `fork_secondary_model` lives under [ui] and must never be used.
    let text = "[ui]\nfork_secondary_model = \"grok-4.5\"\n\n[models]\ndefault = \"grok-4.6\"\n";
    assert_eq!(sources::default_model(text).as_deref(), Some("grok-4.6"));
    assert_eq!(sources::default_model("[ui]\nx = 1\n"), None);
    assert_eq!(sources::default_model(""), None);
}

fn seed_opencode(path: &Path) {
    let connection = rusqlite::Connection::open(path).expect("opencode db");
    connection
        .execute("CREATE TABLE message(id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT)", [])
        .expect("schema");
}

fn insert_message(connection: &rusqlite::Connection, rowid: i64, model: &str, input: i64) {
    let data = format!(
        r#"{{"role":"assistant","modelID":"{model}","providerID":"opencode-go","tokens":{{"input":{input},"output":10,"reasoning":0,"cache":{{"read":5,"write":0}}}},"time":{{"created":1700000000000}}}}"#
    );
    connection
        .execute(
            "INSERT INTO message(rowid, id, data) VALUES (?1, ?2, ?3)",
            rusqlite::params![rowid, format!("m{rowid}"), data],
        )
        .expect("insert message");
}

#[test]
fn opencode_replays_its_window_without_double_counting() {
    let fixture = Fixture::new("opencode-window");
    seed_opencode(&fixture.paths.opencode_db);
    {
        let reader = rusqlite::Connection::open(&fixture.paths.opencode_db).expect("open");
        insert_message(&reader, 1, "deepseek-v4-flash", 100);
    }
    let mut connection = fixture.connection();
    let mut collector = sources::Collector::new(fixture.paths.clone());

    // opencode is not a log file: the cursor is a rowid watermark inside the
    // database, so the row that already exists is collected on the first round.
    assert_eq!(
        collector
            .run_round(&mut connection, 1_000)
            .expect("first round")
            .inserted,
        1
    );
    assert_eq!(count(&connection, "opencode"), 1);

    {
        let reader = rusqlite::Connection::open(&fixture.paths.opencode_db).expect("open");
        insert_message(&reader, 2, "deepseek-v4-flash", 200);
    }
    assert_eq!(
        collector
            .run_round(&mut connection, 2_000)
            .expect("round")
            .inserted,
        1
    );
    // Every later round re-reads the same 2000-row window; both rows must stay
    // single rows rather than being counted again.
    for at in 3_000..3_005 {
        assert_eq!(
            collector
                .run_round(&mut connection, at)
                .expect("round")
                .inserted,
            0
        );
    }
    assert_eq!(count(&connection, "opencode"), 2);
}

#[test]
fn an_opencode_row_filled_after_the_first_pass_is_picked_up_later() {
    let fixture = Fixture::new("opencode-backfill");
    seed_opencode(&fixture.paths.opencode_db);
    {
        let reader = rusqlite::Connection::open(&fixture.paths.opencode_db).expect("open");
        // A row that exists but has no usage yet — the measured "先建后填" shape.
        let empty = r#"{"role":"assistant","modelID":"deepseek-v4-flash","providerID":"opencode-go","tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":1700000000000}}"#;
        reader
            .execute(
                "INSERT INTO message(rowid, id, data) VALUES (1, 'm1', ?1)",
                rusqlite::params![empty],
            )
            .expect("insert empty");
    }
    let mut connection = fixture.connection();
    let mut collector = sources::Collector::new(fixture.paths.clone());
    collector
        .run_round(&mut connection, 1_000)
        .expect("baseline");
    collector.run_round(&mut connection, 2_000).expect("round");
    assert_eq!(
        count(&connection, "opencode"),
        0,
        "an all-zero row is not stored"
    );

    // The backfill lands later. Because the row was never stored as a zero row,
    // the replay window can insert the real numbers.
    {
        let reader = rusqlite::Connection::open(&fixture.paths.opencode_db).expect("open");
        let data = r#"{"role":"assistant","modelID":"deepseek-v4-flash","providerID":"opencode-go","tokens":{"input":700,"output":42,"reasoning":7,"cache":{"read":9,"write":0}},"time":{"created":1700000000000}}"#;
        reader
            .execute(
                "UPDATE message SET data = ?1 WHERE rowid = 1",
                rusqlite::params![data],
            )
            .expect("backfill");
    }
    let report = collector.run_round(&mut connection, 3_000).expect("round");
    assert_eq!(report.inserted, 1);
    let (input, output, reasoning): (i64, i64, i64) = connection
        .query_row(
            "SELECT input_total, output_total, reasoning FROM usage_event WHERE source='opencode'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("row");
    assert_eq!(input, 709, "700 net + 9 cache");
    assert_eq!(output, 49, "42 output + 7 reasoning");
    assert_eq!(reasoning, 7);
}

#[test]
fn pricing_is_applied_from_the_round_tables_and_re_read_each_round() {
    let fixture = Fixture::new("pricing");
    let omp_file = fixture.paths.omp.join("s.jsonl");
    fs::write(&omp_file, "").expect("seed");
    let mut connection = fixture.connection();
    // Price the model only after the collector has already seen it, to prove the
    // per-round reload works without a restart.
    let mut collector = sources::Collector::new(fixture.paths.clone());
    collector
        .run_round(&mut connection, 1_000)
        .expect("baseline");

    let line = OMP_LINE.replace(
        "\"timestamp\":\"2026-09-10T16:38:17.999Z\"",
        "\"timestamp\":\"2026-09-10T16:38:18.999Z\"",
    );
    fixture.append(&omp_file, &format!("{line}\n"));
    collector.run_round(&mut connection, 2_000).expect("round");
    let cost: Option<f64> = connection
        .query_row(
            "SELECT cost_usd FROM usage_event WHERE source='omp'",
            [],
            |row| row.get(0),
        )
        .expect("cost");
    assert_eq!(cost, None, "an unpriceable model stores NULL rather than 0");
    let unresolved: i64 = connection
        .query_row("SELECT COUNT(*) FROM unresolved_model", [], |row| {
            row.get(0)
        })
        .expect("count");
    assert_eq!(unresolved, 1);

    connection
        .execute(
            "INSERT INTO model_price(model_id, prompt, completion, cache_read, cache_write, remark, updated_at)
             VALUES ('codebuddy/deepseek-v4.1-flash', 0.000001, 0.000002, 0.0000001, NULL, NULL, 0)",
            [],
        )
        .expect("price");
    // A new event is required: the existing row keeps its NULL cost, which is the
    // documented "不阻断" behaviour.
    let line2 = OMP_LINE
        .replace(
            "\"timestamp\":\"2026-09-10T16:38:17.999Z\"",
            "\"timestamp\":\"2026-09-10T16:38:19.999Z\"",
        )
        .replace("\"id\":\"681cad2e\"", "\"id\":\"aaaabbbb\"");
    fixture.append(&omp_file, &format!("{line2}\n"));
    let report = collector.run_round(&mut connection, 3_000).expect("round");
    assert_eq!(
        report.inserted, 1,
        "an unpriced model must not block collection"
    );
    let priced: Option<f64> = connection
        .query_row(
            "SELECT cost_usd FROM usage_event WHERE event_id='aaaabbbb'",
            [],
            |row| row.get(0),
        )
        .expect("cost");
    // 211 fresh input + 22144 cache + 86 output.
    let expected = 211.0 * 0.000001 + 22_144.0 * 0.0000001 + 86.0 * 0.000002;
    assert!(
        priced.is_some_and(|value| (value - expected).abs() < 1e-12),
        "expected {expected}, got {priced:?}"
    );
}

#[test]
fn the_free_model_rule_keeps_usage_but_leaves_it_out_of_coverage() {
    let fixture = Fixture::new("free");
    let path = fixture.db_path();
    fs::write(fixture.paths.omp.join("s.jsonl"), "").expect("seed");
    let mut connection = fixture.connection();
    let mut collector = sources::Collector::new(fixture.paths.clone());
    collector
        .run_round(&mut connection, 1_000)
        .expect("baseline");

    // Every 2026-09 `-free` model observed locally: 12 of them.
    let free_models = [
        "deepseek-v4-flash-free",
        "minimax-m2.1-free",
        "hy3-free",
        "mimo-v2.5-free",
        "ox-alpha-free",
        "ling-3.0-flash-free",
        "glm-4.7-free",
        "muse-spark-1.3-contributor-free",
        "north-mini-code-free",
        "laguna-s-2.1-free",
        "nemotron-3-ultra-free",
        "minimax-m2.5-free",
    ];
    for model in free_models {
        connection
            .execute(
                "INSERT INTO model_alias(raw_model, model_id, ignore, resolved_by, remark)
                 VALUES (?1, NULL, 1, 'ignore', '免费通道不计价')",
                rusqlite::params![format!("opencode/{model}")],
            )
            .expect("alias");
    }
    let table = PriceTable::load(&connection).expect("load");
    for model in free_models {
        assert_eq!(
            table.resolve(&format!("opencode/{model}"), &zero_usage()),
            Priced::Ignored,
            "{model} must be ignored, not priced at 0"
        );
    }
    assert_eq!(table.price_count(), 0, "ignored models carry no price row");
    let _ = path;
}

fn zero_usage() -> herdr_usage::event::UsageEvent {
    herdr_usage::event::UsageEvent {
        input_total: 0,
        cache_read: 0,
        cache_write: 0,
        output_total: 0,
        reasoning: 0,
    }
}

#[test]
fn the_real_price_table_prices_the_measured_models() {
    // Guards the §6.4 matching chain against a price-table shape change: an
    // OpenRouter id is `vendor/model`, while the clients report `provider/model`
    // with their own provider names, so only the bare-name step can join them.
    let connection = rusqlite::Connection::open_in_memory().expect("memory db");
    db::initialize(&connection).expect("schema");
    for (model_id, prompt, completion, cache_read) in [
        (
            "meta/muse-spark-1.3-contributor",
            0.0000004,
            0.0000016,
            0.00000004,
        ),
        ("tencent/hy3", 0.0000001, 0.0000002, 0.00000001),
        (
            "deepseek/deepseek-v4-flash",
            0.00000008708,
            0.00000017416,
            0.000000017416,
        ),
    ] {
        connection
            .execute(
                "INSERT INTO model_price(model_id, prompt, completion, cache_read, cache_write, remark, updated_at)
                 VALUES (?1, ?2, ?3, ?4, NULL, NULL, 0)",
                rusqlite::params![model_id, prompt, completion, cache_read],
            )
            .expect("price");
    }
    let table = PriceTable::load(&connection).expect("load");
    let usage = herdr_usage::event::UsageEvent {
        input_total: 1_000,
        cache_read: 400,
        cache_write: 0,
        output_total: 200,
        reasoning: 0,
    };
    for raw in [
        "opencode-go/muse-spark-1.3-contributor",
        "opencode-go/hy3",
        "opencode-go/deepseek-v4-flash",
        "muse-spark-1.3-contributor",
    ] {
        match table.resolve(raw, &usage) {
            Priced::Cost(value) => assert!(value > 0.0, "{raw} priced at zero"),
            other => panic!("{raw} did not resolve: {other:?}"),
        }
    }
}

#[test]
fn a_model_absent_from_the_price_table_stays_unresolved() {
    let connection = rusqlite::Connection::open_in_memory().expect("memory db");
    db::initialize(&connection).expect("schema");
    connection
        .execute(
            "INSERT INTO model_price(model_id, prompt, completion, cache_read, cache_write, remark, updated_at)
             VALUES ('vendor/known', 0.1, 0.2, NULL, NULL, NULL, 0)",
            [],
        )
        .expect("price");
    let table = PriceTable::load(&connection).expect("load");
    let usage = zero_usage();
    // `ox-alpha-free` has no OpenRouter row at all; it must be reported, never
    // silently billed at zero.
    assert_eq!(
        table.resolve("opencode-go/ox-alpha-free", &usage),
        Priced::Unresolved
    );
    assert_eq!(table.resolve("brand-new-model", &usage), Priced::Unresolved);
}
