//! Per-round collection: walk each source, parse new bytes, price, persist.
//!
//! Everything here is driven by the watermarks in `collect_offset`, so a restart
//! resumes exactly where the previous process stopped (plan §7.2 "启动水位": the
//! first ever run records the current position and backfills nothing).

pub mod codex;
pub mod grok;
pub mod omp;
pub mod opencode;

use crate::{
    cost::{self, PriceTable, Priced},
    db,
    event::ParsedEvent,
    tail::{self, OffsetStore},
};
use rusqlite::Connection;
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

/// Where the four sources live. Every field is overridable so tests can point at
/// a fixture directory instead of the live logs.
#[derive(Clone, Debug)]
pub struct SourcePaths {
    pub omp: PathBuf,
    /// `~/.omp/profiles`; each `<profile>/agent/sessions` is a second omp
    /// account's session tree (the `pro2` profile is a separate login).
    pub omp_profiles: PathBuf,
    pub codex: PathBuf,
    pub grok_log: PathBuf,
    pub grok_config: PathBuf,
    pub opencode_db: PathBuf,
}

impl SourcePaths {
    pub fn from_home(home: &Path) -> Self {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        Self {
            omp: home.join(".omp/agent/sessions"),
            omp_profiles: home.join(".omp/profiles"),
            codex: home.join(".codex/sessions"),
            grok_log: home.join(".grok/logs/unified.jsonl"),
            grok_config: home.join(".grok/config.toml"),
            opencode_db: data.join("opencode/opencode.db"),
        }
    }

    /// Every omp session root: the primary one plus each profile that has a
    /// session tree. Profile directories are enumerated in sorted order so two
    /// runs visit the same files in the same sequence (the watermark is keyed by
    /// absolute path, so ordering only affects determinism of the round report).
    pub fn omp_session_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![self.omp.clone()];
        let Ok(entries) = std::fs::read_dir(&self.omp_profiles) else {
            return roots;
        };
        let mut profiles: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("agent/sessions"))
            .filter(|path| path.is_dir())
            .collect();
        profiles.sort();
        roots.extend(profiles);
        roots
    }
}

/// Mutable state that must survive between rounds.
pub struct Collector {
    paths: SourcePaths,
    omp: omp::OmpState,
    codex: codex::CodexState,
    grok: grok::GrokState,
    /// `config.toml` mtime of the last successful read, so the fallback model is
    /// re-read only when the file actually changed (plan §2.5).
    grok_config_mtime: Option<SystemTime>,
    /// Files visited in the most recent omp walk, for the round report.
    scanned_files: usize,
}

/// What one round accomplished, for logging.
#[derive(Debug, Default)]
pub struct RoundReport {
    pub inserted: usize,
    pub scanned_files: usize,
}

impl Collector {
    pub fn new(paths: SourcePaths) -> Self {
        Self {
            paths,
            omp: omp::OmpState::new(),
            codex: codex::CodexState::new(),
            grok: grok::GrokState::new(None),
            grok_config_mtime: None,
            scanned_files: 0,
        }
    }

    /// Run one collection round.
    pub fn run_round(
        &mut self,
        connection: &mut Connection,
        now: i64,
    ) -> rusqlite::Result<RoundReport> {
        let mut report = RoundReport::default();
        // Both tables are re-read every round so a price update takes effect
        // within one tick, without a restart that could disturb the watermarks.
        let prices = PriceTable::load(connection)?;

        report.inserted += self.round_omp(connection, &prices, now)?;
        report.inserted += self.round_codex(connection, &prices, now)?;
        report.inserted += self.round_grok(connection, &prices, now)?;
        report.inserted += self.round_opencode(connection, &prices, now)?;
        report.scanned_files = self.scanned_files;
        Ok(report)
    }

    fn round_omp(
        &mut self,
        connection: &mut Connection,
        prices: &PriceTable,
        now: i64,
    ) -> rusqlite::Result<usize> {
        let kind = "omp";
        let roots = self.paths.omp_session_roots();
        let catalogs: Vec<_> = roots
            .iter()
            .map(|root| {
                let database = root.parent().unwrap_or(root).join("agent.db");
                (root.clone(), omp::AccountCatalog::load(&database))
            })
            .collect();
        let mut files = Vec::new();
        for root in &roots {
            files.extend(omp::session_files(root));
        }
        self.scanned_files = files.len();
        let mut store = OffsetStore::from_cursor(db::offset(connection, kind)?.as_deref());
        let mut batch = vec![];
        let mut inserted = 0;
        for file in &files {
            self.omp.seed_file(file);
            let accounts = catalogs
                .iter()
                .find(|(root, _)| file.starts_with(root))
                .map(|(_, accounts)| accounts)
                .expect("OMP session file belongs to an enumerated root");
            read_new_chunks(&mut store, file, |lines, rotated| {
                if rotated {
                    self.omp.forget(file);
                }
                for line in lines {
                    for event in self.omp.parse_line_with_accounts(file, &line, accounts) {
                        let priced = price(prices, &event);
                        batch.push((event, priced));
                    }
                }
                for event in self.omp.flush_file(file, accounts) {
                    let priced = price(prices, &event);
                    batch.push((event, priced));
                }
                if batch.len() >= 1000 {
                    inserted += db::insert_events(connection, kind, &batch, now)?;
                    batch.clear();
                }
                Ok(())
            })?;
        }
        inserted += db::insert_events(connection, kind, &batch, now)?;
        db::save_offset(connection, kind, &store.cursor())?;
        Ok(inserted)
    }

    fn round_codex(
        &mut self,
        connection: &mut Connection,
        prices: &PriceTable,
        now: i64,
    ) -> rusqlite::Result<usize> {
        let kind = "codex";
        let files = tail::collect_files(&self.paths.codex, |path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
        });
        let mut store = OffsetStore::from_cursor(db::offset(connection, kind)?.as_deref());
        let mut batch = vec![];
        let mut inserted = 0;
        for file in &files {
            read_new_chunks(&mut store, file, |lines, rotated| {
                if rotated {
                    self.codex.forget(file);
                }
                for line in lines {
                    for event in self.codex.parse_line(file, &line) {
                        let priced = price(prices, &event);
                        batch.push((event, priced));
                    }
                }
                if batch.len() >= 1000 {
                    inserted += db::insert_events(connection, kind, &batch, now)?;
                    batch.clear();
                }
                Ok(())
            })?;
        }
        inserted += db::insert_events(connection, kind, &batch, now)?;
        db::save_offset(connection, kind, &store.cursor())?;
        Ok(inserted)
    }

    fn round_grok(
        &mut self,
        connection: &mut Connection,
        prices: &PriceTable,
        now: i64,
    ) -> rusqlite::Result<usize> {
        let kind = "grok";
        self.refresh_grok_config();
        let mut store = OffsetStore::from_cursor(db::offset(connection, kind)?.as_deref());
        let mut batch = vec![];
        let mut inserted = 0;
        let file = self.paths.grok_log.clone();
        read_new_chunks(&mut store, &file, |lines, rotated| {
            if rotated {
                // Single-file source: rotation invalidates every `sid -> model`
                // mapping, since the new file describes new sessions.
                self.grok.forget(&file);
            }
            for line in lines {
                for event in self.grok.parse_line(&file, &line) {
                    let priced = price(prices, &event);
                    batch.push((event, priced));
                }
            }
            if batch.len() >= 1000 {
                inserted += db::insert_events(connection, kind, &batch, now)?;
                batch.clear();
            }
            Ok(())
        })?;
        inserted += db::insert_events(connection, kind, &batch, now)?;
        db::save_offset(connection, kind, &store.cursor())?;
        Ok(inserted)
    }

    fn round_opencode(
        &mut self,
        connection: &mut Connection,
        prices: &PriceTable,
        now: i64,
    ) -> rusqlite::Result<usize> {
        let kind = "opencode";
        if !self.paths.opencode_db.exists() {
            return Ok(0);
        }
        let reader = match opencode::open(&self.paths.opencode_db) {
            Ok(reader) => reader,
            Err(error) => {
                eprintln!("[warn] opencode 读取失败：{error}");
                return Ok(0);
            }
        };
        let max = opencode::max_rowid(&reader);
        // The cursor's `max_rowid` is the never-backfill baseline recorded at
        // the first ever round; it stays fixed for the life of the source. A
        // first round (no cursor yet) records the current top rowid and reads
        // nothing, exactly like the file sources' baseline. Without it the
        // 2000-row replay window would import the last stretch of
        // pre-collector history on the first ever run. Later rounds read with
        // `replay_floor`, which never reaches below the baseline yet still
        // re-reads the last `REPLAY_WINDOW` rows for late backfills.
        let (baseline, floor) = match db::offset(connection, kind)?
            .as_deref()
            .and_then(opencode::baseline_from_cursor)
        {
            Some(baseline) => (baseline, opencode::replay_floor(baseline, max)),
            None => {
                let baseline = max;
                db::save_offset(connection, kind, &format!("{{\"max_rowid\":{baseline}}}"))?;
                (baseline, max)
            }
        };
        let window = match opencode::read_window(&reader, floor) {
            Ok(window) => window,
            Err(error) => {
                eprintln!("[warn] opencode 查询失败：{error}");
                return Ok(0);
            }
        };
        let mut batch = vec![];
        for row in &window {
            let Some(event) = opencode::parse_message(row) else {
                continue;
            };
            let priced = price(prices, &event);
            batch.push((event, priced));
        }
        let inserted = db::insert_events(connection, kind, &batch, now)?;
        // The baseline never moves; rewriting it here is idempotent and keeps
        // the cursor write in the same round as the inserts.
        db::save_offset(connection, kind, &format!("{{\"max_rowid\":{baseline}}}"))?;
        Ok(inserted)
    }

    /// Re-read `[models].default` when `config.toml`'s mtime moves (plan §2.5).
    fn refresh_grok_config(&mut self) {
        let mtime = std::fs::metadata(&self.paths.grok_config)
            .and_then(|meta| meta.modified())
            .ok();
        if mtime == self.grok_config_mtime {
            return;
        }
        self.grok_config_mtime = mtime;
        let model = std::fs::read_to_string(&self.paths.grok_config)
            .ok()
            .and_then(|text| default_model(&text));
        self.grok.set_default_model(model);
    }
}

/// Pull the `[models].default` value out of a grok `config.toml`.
///
/// Deliberately a tiny hand-rolled reader rather than a TOML dependency: the
/// only key that matters is one string, and `fork_secondary_model` (which lives
/// under `[ui]`) must NOT be picked up.
pub fn default_model(text: &str) -> Option<String> {
    let mut section = String::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            section = name.trim().to_owned();
            continue;
        }
        if section != "models" {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "default" {
            continue;
        }
        let value = value.trim().trim_matches('"').trim_matches('\'');
        if !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    None
}

fn price(prices: &PriceTable, event: &ParsedEvent) -> Priced {
    match &event.model {
        Some(model) => prices.resolve(model, &event.usage),
        None => Priced::Unresolved,
    }
}

/// Fetch the file size, then read only the bytes appended since the watermark.
///
/// Two kinds of files carry no watermark, and they must not be treated alike:
///
/// * A file whose last write is older than `SEED_ACTIVE` is *baselined*: the
///   store records its current size and reads nothing. That is the "不回填历史"
///   rule (plan §7.2) — without it, pointing the collector at an existing corpus
///   would import every byte of history on the first round.
///
/// * A file written within the last `SEED_ACTIVE` is a *live* file the collector
///   has simply never seen. omp buffers session writes, so a log can be created
///   minutes after its session started and already hold collected events;
///   baselining such a file would silently drop those events forever. It is
///   read from zero instead — harmless even if some bytes were somehow already
///   consumed, because every insert is `INSERT OR IGNORE` over the primary key.
///
/// How recent a write makes an unseen file "live". One collection round plus
/// change: a file seen in an earlier round holds a watermark and never reaches
/// the baseline branch at all.
const SEED_ACTIVE: Duration = Duration::from_secs(120);

/// Whether the file's mtime is recent enough to treat it as live.
fn is_recent(path: &Path, within: Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age <= within)
}

/// Returns `None` when the file vanished, is not a regular file, or fails to
/// stat; the round then simply skips it.
fn read_new_chunks(
    store: &mut OffsetStore,
    path: &Path,
    mut consume: impl FnMut(Vec<tail::TailLine>, bool) -> rusqlite::Result<()>,
) -> rusqlite::Result<()> {
    let Ok(metadata) = std::fs::metadata(path) else {
        return Ok(());
    };
    if !metadata.is_file() {
        return Ok(());
    }
    let size = metadata.len();
    let size = if store.is_known(path) {
        size
    } else if store.is_resumed() && is_recent(path, SEED_ACTIVE) {
        // A live file seen for the first time after a previous round: adopt it
        // at offset zero so the events it was born with are collected. omp
        // writes session logs lazily, so a newly listed file may already hold
        // inference records — baselining at its current size used to drop them
        // permanently. Nothing is skipped, so the model state the skipped prefix
        // would have established is established by the read itself. A cold
        // start (no cursor) keeps the baseline: its unseen corpus is history by
        // definition, not a log that a running session is still filling.
        store.baseline(path, 0);
        size
    } else {
        // Recover source metadata only when new usage actually needs it.
        store.baseline(path, size)
    };
    loop {
        match tail::read_appended(path, store.tail(path), size) {
            Ok(outcome) => {
                let done = outcome.offset >= size;
                consume(outcome.lines, outcome.rotated)?;
                if done {
                    return Ok(());
                }
            }
            Err(error) => {
                eprintln!("[warn] 读取 {} 失败：{error}", path.display());
                return Ok(());
            }
        }
    }
}

/// Wall-clock helper shared by the binary and the tests.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Bounded logging cadence for the main loop.
pub const ROUND: Duration = Duration::from_secs(60);

/// Read `note_unresolved` for every unpriced event in a batch and report the
/// unresolved names, so the caller can log the reminder list origin.
pub fn unresolved_names(connection: &Connection, since_ms: i64) -> Vec<String> {
    cost::unresolved_within(connection, since_ms)
        .map(|rows| rows.into_iter().map(|row| row.raw_model).collect())
        .unwrap_or_default()
}

/// Fill in `provider` on the omp rows written before that column existed.
///
/// The events already in the database carry no provider and cannot be
/// re-derived from the watermark (those bytes were consumed long ago), so the
/// session logs are re-read once. Only rows that actually need it are searched
/// for, and only files whose mtime is newer than the oldest such row are opened:
/// a file's mtime is never earlier than its last event, so an older file cannot
/// contain the line. That pruning is what keeps this from walking the whole
/// multi-gigabyte session corpus.
///
/// Runs inside one transaction and returns the number of rows updated.
pub fn backfill_omp_provider(
    connection: &mut Connection,
    roots: &[PathBuf],
) -> rusqlite::Result<usize> {
    use std::{
        collections::{HashMap, HashSet},
        io::{BufRead, BufReader},
    };

    let mut pending: HashSet<String> = {
        let mut statement = connection.prepare(
            "SELECT event_id FROM usage_event WHERE source = 'omp' AND provider IS NULL",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.filter_map(Result::ok).collect()
    };
    if pending.is_empty() {
        return Ok(0);
    }
    let floor_ms: i64 = connection.query_row(
        "SELECT MIN(occurred_at) FROM usage_event WHERE source = 'omp' AND provider IS NULL",
        [],
        |row| row.get(0),
    )?;

    let mut found: HashMap<String, String> = HashMap::new();
    'roots: for root in roots {
        for file in omp::session_files(root) {
            if pending.is_empty() {
                break 'roots;
            }
            let Some(mtime_ms) = std::fs::metadata(&file)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|since| since.as_millis() as i64)
            else {
                continue;
            };
            if mtime_ms < floor_ms - 3_600_000 {
                continue;
            }
            let Ok(handle) = std::fs::File::open(&file) else {
                continue;
            };
            for line in BufReader::new(handle).split(b'\n') {
                let Ok(line) = line else { break };
                // Cheap reject: only assistant records carry a provider.
                if !line.windows(7).any(|window| window == b"\"usage\"") {
                    continue;
                }
                let Some((id, provider)) = omp::record_provider(&line) else {
                    continue;
                };
                if pending.remove(&id) {
                    found.insert(id, provider);
                }
            }
        }
    }
    if found.is_empty() {
        return Ok(0);
    }

    let transaction = connection.transaction()?;
    let mut updated = 0usize;
    {
        let mut statement = transaction.prepare(
            "UPDATE usage_event SET provider = ?1
             WHERE source = 'omp' AND event_id = ?2 AND provider IS NULL",
        )?;
        for (id, provider) in &found {
            updated += statement.execute(rusqlite::params![provider, id])?;
        }
    }
    transaction.commit()?;
    Ok(updated)
}

/// Enrich already-collected OMP usage rows with request/session metadata.
/// Only structural lines and assistant usage records are parsed; no prompt,
/// response, or tool content is written to the usage database.
pub fn backfill_omp_request_metadata(
    connection: &mut Connection,
    roots: &[PathBuf],
) -> rusqlite::Result<usize> {
    use std::{
        collections::{HashMap, HashSet},
        io::{BufRead, BufReader},
    };

    let mut pending: HashSet<String> = {
        let mut statement = connection.prepare(
            "SELECT event_id FROM usage_event
             WHERE source='omp' AND session_id IS NULL
               AND provider IN ('google-antigravity','opencode-go')",
        )?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(Result::ok)
            .collect();
        rows
    };
    if pending.is_empty() {
        return Ok(0);
    }
    let floor_ms: i64 = connection.query_row(
        "SELECT MIN(occurred_at) FROM usage_event
         WHERE source='omp' AND session_id IS NULL
           AND provider IN ('google-antigravity','opencode-go')",
        [],
        |row| row.get(0),
    )?;
    let mut found = HashMap::<String, ParsedEvent>::new();
    let mut state = omp::OmpState::new();
    'roots: for root in roots {
        let catalog = omp::AccountCatalog::load(&root.parent().unwrap_or(root).join("agent.db"));
        for file in omp::session_files(root) {
            if pending.is_empty() {
                break 'roots;
            }
            let Some(mtime_ms) = std::fs::metadata(&file)
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis() as i64)
            else {
                continue;
            };
            if mtime_ms < floor_ms.saturating_sub(3_600_000) {
                continue;
            }
            let Ok(handle) = std::fs::File::open(&file) else {
                continue;
            };
            for (ordinal, line) in BufReader::new(handle).split(b'\n').enumerate() {
                let Ok(line) = line else { break };
                if !line.windows(9).any(|part| part == b"\"session\"")
                    && !line.windows(14).any(|part| part == b"credential_pin")
                    && !line.windows(14).any(|part| part == b"reset_boundary")
                    && !line
                        .windows(b"herdr-api-key-sticky-v1".len())
                        .any(|part| part == b"herdr-api-key-sticky-v1")
                    && !line.windows(7).any(|part| part == b"\"usage\"")
                {
                    continue;
                }
                let tail = tail::TailLine {
                    start: ordinal as u64,
                    bytes: line,
                };
                for event in state.parse_line_with_accounts(&file, &tail, &catalog) {
                    if pending.remove(&event.event_id) {
                        found.insert(event.event_id.clone(), event);
                    }
                }
            }
            for event in state.flush_file(&file, &catalog) {
                if pending.remove(&event.event_id) {
                    found.insert(event.event_id.clone(), event);
                }
            }
        }
    }
    if found.is_empty() {
        return Ok(0);
    }
    let transaction = connection.transaction()?;
    let mut updated = 0;
    {
        let mut statement = transaction.prepare(
            "UPDATE usage_event SET
                 session_id=?1, started_at=?2, completed_at=?3, duration_ms=?4,
                 account_key=?5, account_label=?6, account_source=?7
             WHERE source='omp' AND event_id=?8 AND session_id IS NULL",
        )?;
        for event in found.values() {
            updated += statement.execute(rusqlite::params![
                event.session_id,
                event.started_at,
                event.completed_at,
                event.duration_ms,
                event.account_key,
                event.account_label,
                event.account_source,
                event.event_id,
            ])?;
        }
    }
    transaction.commit()?;
    Ok(updated)
}

/// Correct OpenCode Go request ownership from the append-only API-key pin
/// timeline emitted by the Herdr OMP extension. Unlike `session:sticky`, these
/// entries preserve rotations within one session. Rows without timeline
/// evidence are deliberately left unchanged.
pub fn backfill_omp_api_key_timeline(
    connection: &mut Connection,
    roots: &[PathBuf],
) -> rusqlite::Result<usize> {
    use std::{
        collections::HashMap,
        io::{BufRead, BufReader},
    };

    let mut found = HashMap::<String, ParsedEvent>::new();
    for root in roots {
        let catalog = omp::AccountCatalog::load(&root.parent().unwrap_or(root).join("agent.db"));
        let mut state = omp::OmpState::new();
        for file in omp::session_files(root) {
            let Ok(handle) = std::fs::File::open(&file) else {
                continue;
            };
            for (ordinal, line) in BufReader::new(handle).split(b'\n').enumerate() {
                let Ok(line) = line else { break };
                if !line.windows(9).any(|part| part == b"\"session\"")
                    && !line
                        .windows(b"herdr-api-key-sticky-v1".len())
                        .any(|part| part == b"herdr-api-key-sticky-v1")
                    && !line.windows(7).any(|part| part == b"\"usage\"")
                {
                    continue;
                }
                let tail = tail::TailLine {
                    start: ordinal as u64,
                    bytes: line,
                };
                for event in state.parse_line_with_accounts(&file, &tail, &catalog) {
                    if event.provider.as_deref() == Some("opencode-go")
                        && event.account_source.as_deref() == Some("session_pin")
                    {
                        found.insert(event.event_id.clone(), event);
                    }
                }
            }
        }
    }
    if found.is_empty() {
        return Ok(0);
    }

    let transaction = connection.transaction()?;
    let mut updated = 0usize;
    {
        let mut statement = transaction.prepare(
            "UPDATE usage_event SET account_key=?1, account_label=?2, account_source=?3
             WHERE source='omp' AND provider='opencode-go' AND event_id=?4",
        )?;
        for event in found.values() {
            updated += statement.execute(rusqlite::params![
                event.account_key,
                event.account_label,
                event.account_source,
                event.event_id,
            ])?;
        }
    }
    transaction.commit()?;
    Ok(updated)
}
