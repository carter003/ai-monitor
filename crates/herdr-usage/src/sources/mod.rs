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
            codex: home.join(".codex/sessions"),
            grok_log: home.join(".grok/logs/unified.jsonl"),
            grok_config: home.join(".grok/config.toml"),
            opencode_db: data.join("opencode/opencode.db"),
        }
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
        let files = omp::session_files(&self.paths.omp);
        self.scanned_files = files.len();
        let mut store = OffsetStore::from_cursor(db::offset(connection, kind)?.as_deref());
        let mut batch = vec![];
        for file in &files {
            let Some((size, lines, rotated)) = read_new_bytes(&mut store, file, |_, _| {})? else {
                continue;
            };
            if rotated {
                self.omp.forget(file);
            }
            for line in lines {
                for event in self.omp.parse_line(file, &line) {
                    let priced = price(prices, &event);
                    batch.push((event, priced));
                }
            }
            let _ = size;
        }
        let inserted = db::insert_events(connection, kind, &batch, now)?;
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
        for file in &files {
            let Some((_, lines, rotated)) = read_new_bytes(&mut store, file, |path, prefix| {
                self.codex.seed_model(path, prefix);
            })?
            else {
                continue;
            };
            if rotated {
                // A rotated rollout file restarts its `turn_context` history; the
                // tracked model must not leak across the truncation.
                self.codex.forget(file);
            }
            for line in lines {
                for event in self.codex.parse_line(file, &line) {
                    let priced = price(prices, &event);
                    batch.push((event, priced));
                }
            }
        }
        let inserted = db::insert_events(connection, kind, &batch, now)?;
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
        let file = self.paths.grok_log.clone();
        if let Some((_, lines, rotated)) = read_new_bytes(&mut store, &file, |_, _| {})? {
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
        }
        let inserted = db::insert_events(connection, kind, &batch, now)?;
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
        let window = match opencode::read_window(&reader, max) {
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
        // The watermark is the window's top rowid; the replay overlap is applied
        // at read time (`read_window`), not here.
        db::save_offset(connection, kind, &format!("{{\"max_rowid\":{max}}}"))?;
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
/// A file with no watermark is baselined (adopted at its current size without
/// reading) so that the first ever round never backfills history.
///
/// A file untouched for longer than this is not seeded on baseline; it will be
/// seeded normally if it starts growing again.
const SEED_PREFIX_IDLE: Duration = Duration::from_secs(3600);

/// Whether the file's mtime is recent enough to justify reading its prefix.
fn is_recent(path: &Path, within: Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age <= within)
}

/// `on_baseline` runs once for a file with no watermark, with the bytes that the
/// baseline is about to skip. Sources whose parsing is stateful (codex) use it to
/// recover the state that the skipped prefix establishes.
///
/// Returns `None` when the file vanished, is not a regular file, or fails to
/// stat; the round then simply skips it.
fn read_new_bytes(
    store: &mut OffsetStore,
    path: &Path,
    mut on_baseline: impl FnMut(&Path, &[u8]),
) -> rusqlite::Result<Option<(u64, Vec<tail::TailLine>, bool)>> {
    let Ok(metadata) = std::fs::metadata(path) else {
        return Ok(None);
    };
    if !metadata.is_file() {
        return Ok(None);
    }
    let size = metadata.len();
    let size = if store.is_known(path) {
        size
    } else {
        // Reading the whole prefix is exactly what baselining must avoid, so the
        // callback gets a bounded tail of it: the last `SEED_PREFIX` bytes are
        // where the live session's `turn_context` sits, and truncating the prefix
        // can only lose a model that a later `turn_context` restores.
        //
        // The scan is skipped for files that were not written recently. Inactive
        // rollouts are the overwhelming majority (measured 5101 files, of which
        // one was touched in the last hour), and their model is never needed
        // until a later round reads new bytes from them.
        if is_recent(path, SEED_PREFIX_IDLE) {
            const SEED_PREFIX: u64 = 1 << 20;
            let from = size.saturating_sub(SEED_PREFIX);
            if let Ok(prefix) = tail::read_range(path, from, size) {
                on_baseline(path, &prefix);
            }
        } else {
            on_baseline(path, &[]);
        }
        store.baseline(path, size)
    };
    let tail = store.tail(path);
    match tail::read_appended(path, tail, size) {
        Ok(outcome) => Ok(Some((outcome.offset, outcome.lines, outcome.rotated))),
        Err(error) => {
            eprintln!("[warn] 读取 {} 失败：{error}", path.display());
            Ok(None)
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
