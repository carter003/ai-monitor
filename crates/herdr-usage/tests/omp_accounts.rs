use herdr_usage::{db, sources};
use rusqlite::Connection;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

struct Fixture {
    _home: tempfile::TempDir,
    paths: sources::SourcePaths,
    auth: Connection,
    usage: Connection,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let root = home.path();
        let paths = sources::SourcePaths {
            omp: root.join(".omp/agent/sessions"),
            omp_profiles: root.join(".omp/profiles"),
            codex: root.join(".codex/sessions"),
            grok_log: root.join(".grok/logs/unified.jsonl"),
            grok_config: root.join(".grok/config.toml"),
            opencode_db: root.join("opencode/opencode.db"),
        };
        fs::create_dir_all(&paths.omp).unwrap();
        let auth = Connection::open(paths.omp.parent().unwrap().join("agent.db")).unwrap();
        auth.execute_batch("CREATE TABLE auth_credentials(id INTEGER PRIMARY KEY,provider TEXT,credential_type TEXT,data TEXT); CREATE TABLE cache(key TEXT PRIMARY KEY,value TEXT); INSERT INTO auth_credentials VALUES(7,'opencode-go','api_key','{\"key\":\"account-a\"}'),(8,'opencode-go','api_key','{\"key\":\"account-b\"}');").unwrap();
        let usage = db::open(&root.join("usage.db")).unwrap();
        Self {
            _home: home,
            paths,
            auth,
            usage,
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        let file = self.paths.omp.join(name);
        fs::write(&file, "").unwrap();
        file
    }
}

fn append(file: &Path, lines: &[String]) {
    let mut file = fs::OpenOptions::new().append(true).open(file).unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
}
fn header(session: &str) -> String {
    format!(r#"{{"type":"session","id":"{session}"}}"#)
}
fn pin(session: &str, credential: i64, at: i64) -> String {
    format!(
        r#"{{"type":"custom","customType":"herdr-api-key-sticky-v1","data":{{"action":"pin","provider":"opencode-go","sessionId":"{session}","credentialId":{credential},"at":{at}}}}}"#
    )
}
fn request(id: &str, at: i64) -> String {
    format!(
        r#"{{"type":"message","id":"{id}","message":{{"role":"assistant","provider":"opencode-go","model":"mimo-v2.6-flash","timestamp":{at},"duration":10,"usage":{{"input":1,"output":2}}}}}}"#
    )
}
fn accounts(connection: &Connection) -> Vec<(String, Option<String>, Option<String>)> {
    connection.prepare("SELECT event_id,account_key,account_source FROM usage_event ORDER BY occurred_at,event_id").unwrap()
        .query_map([], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap()
        .collect::<rusqlite::Result<_>>().unwrap()
}

#[test]
fn sibling_file_pins_survive_mutable_sticky_and_repair_old_rows() {
    let mut f = Fixture::new();
    let parent = f.file("parent.jsonl");
    let child = f.file("a-child.jsonl");
    let writer = f.file("z-sibling.jsonl");
    let mut collector = sources::Collector::new(f.paths.clone());
    collector.run_round(&mut f.usage, 0).unwrap();
    append(
        &parent,
        &[
            header("parent"),
            pin("parent", 8, 100),
            request("parent-first", 200),
        ],
    );
    append(&child, &[header("child"), request("child-first", 210)]);
    append(
        &writer,
        &[
            header("sibling"),
            pin("child", 7, 110),
            pin("sibling", 8, 120),
            request("sibling-first", 220),
        ],
    );
    collector.run_round(&mut f.usage, 300).unwrap();
    f.auth
        .execute(
            "INSERT INTO cache VALUES('session:sticky:opencode-go:child','{\"credentialId\":8}')",
            [],
        )
        .unwrap();
    append(&child, &[request("child-resumed", 400)]);
    collector.run_round(&mut f.usage, 500).unwrap();
    let before = accounts(&f.usage);
    assert_ne!(before[0].1, before[1].1);
    assert_eq!(before[1].1, before[3].1);
    assert_eq!(before[0].1, before[2].1);
    assert!(before
        .iter()
        .all(|row| row.2.as_deref() == Some("session_pin")));
    let totals: (i64, i64, i64) = f
        .usage
        .query_row(
            "SELECT COUNT(*),SUM(input_total),SUM(output_total) FROM usage_event",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    f.usage.execute("UPDATE usage_event SET account_key='wrong',account_label='wrong',account_source='sticky_cache' WHERE session_id='child'",[]).unwrap();
    assert_eq!(
        sources::backfill_omp_api_key_timeline(&mut f.usage, std::slice::from_ref(&f.paths.omp))
            .unwrap(),
        2
    );
    assert_eq!(accounts(&f.usage), before);
    assert_eq!(
        sources::backfill_omp_api_key_timeline(&mut f.usage, std::slice::from_ref(&f.paths.omp))
            .unwrap(),
        0
    );
    assert_eq!(
        f.usage
            .query_row(
                "SELECT COUNT(*),SUM(input_total),SUM(output_total) FROM usage_event",
                [],
                |row| Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?
                ))
            )
            .unwrap(),
        totals
    );
}

#[test]
fn a_late_sibling_pin_repairs_an_already_collected_request() {
    let mut f = Fixture::new();
    let child = f.file("a-child.jsonl");
    let writer = f.file("z-sibling.jsonl");
    let mut collector = sources::Collector::new(f.paths.clone());
    collector.run_round(&mut f.usage, 0).unwrap();
    append(&child, &[header("child"), request("request", 200)]);
    append(&writer, &[header("sibling")]);
    collector.run_round(&mut f.usage, 300).unwrap();
    assert_eq!(accounts(&f.usage)[0].1, None);
    append(&writer, &[pin("child", 7, 100)]);
    let report = collector.run_round(&mut f.usage, 400).unwrap();
    assert_eq!(report.inserted, 0);
    let row = &accounts(&f.usage)[0];
    assert!(row
        .1
        .as_deref()
        .is_some_and(|key| key.starts_with("api_key:")));
    assert_eq!(row.2.as_deref(), Some("session_pin"));
}

#[test]
fn current_sticky_and_single_credential_cannot_prove_a_request_account() {
    let mut f = Fixture::new();
    f.auth
        .execute("DELETE FROM auth_credentials WHERE id=8", [])
        .unwrap();
    f.auth
        .execute(
            "INSERT INTO cache VALUES('session:sticky:opencode-go:unknown','{\"credentialId\":7}')",
            [],
        )
        .unwrap();
    let file = f.file("unknown.jsonl");
    let mut collector = sources::Collector::new(f.paths.clone());
    collector.run_round(&mut f.usage, 0).unwrap();
    append(&file, &[header("unknown"), request("request", 200)]);
    collector.run_round(&mut f.usage, 300).unwrap();
    assert_eq!(accounts(&f.usage), vec![("request".to_owned(), None, None)]);
    f.usage.execute("UPDATE usage_event SET account_key='guessed',account_label='guessed',account_source='sticky_cache'",[]).unwrap();
    assert_eq!(
        sources::backfill_omp_api_key_timeline(&mut f.usage, std::slice::from_ref(&f.paths.omp))
            .unwrap(),
        1
    );
    assert_eq!(accounts(&f.usage), vec![("request".to_owned(), None, None)]);
}
