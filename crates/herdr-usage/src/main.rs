//! herdr-usage: resident collector for agent token consumption.
//!
//! Runs one collection round every 60 seconds and never touches the network.
//! The price table is written only by the separate `import_prices` binary.

use herdr_usage::{db, db_path, home_dir, sources};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

/// Set by the SIGINT/SIGTERM handler; read by the main loop.
static STOPPED: AtomicBool = AtomicBool::new(false);

/// `collect_offset` row marking the one-off provider backfill as done. It is not
/// a collection source; the table is reused because the value must survive
/// restarts exactly like a watermark does.
const BACKFILL_KIND: &str = "omp-provider-backfill";
const REQUEST_BACKFILL_KIND: &str = "omp-request-metadata-backfill-v1";
const API_KEY_TIMELINE_BACKFILL_KIND: &str = "omp-api-key-timeline-backfill-v1";

fn main() {
    let database = db_path();
    let home = home_dir();
    let mut connection = match db::open(&database) {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("无法打开数据库 {}：{error}", database.display());
            std::process::exit(1);
        }
    };
    let paths = sources::SourcePaths::from_home(&home);
    install_signal_handlers();

    println!(
        "herdr-usage 启动：db={} omp={} codex={} grok={} opencode={}",
        database.display(),
        paths.omp.display(),
        paths.codex.display(),
        paths.grok_log.display(),
        paths.opencode_db.display()
    );

    let roots = paths.omp_session_roots();
    let mut collector = sources::Collector::new(paths);
    match backfill_once(&mut connection, &roots) {
        Ok(Some(rows)) => println!("provider 回填：{rows} 行"),
        Ok(None) => {}
        // A failure leaves the marker unwritten and retries on the next start
        // rather than blocking collection.
        Err(error) => eprintln!("[warn] provider 回填失败：{error}（下次启动重试）"),
    }
    match request_backfill_once(&mut connection, &roots) {
        Ok(Some(rows)) => println!("request metadata 回填：{rows} 行"),
        Ok(None) => {}
        Err(error) => eprintln!("[warn] request metadata 回填失败：{error}（下次启动重试）"),
    }
    match api_key_timeline_backfill_once(&mut connection, &roots) {
        Ok(Some(rows)) => println!("OpenCode Go 账户时间线回填：{rows} 行"),
        Ok(None) => {}
        Err(error) => eprintln!("[warn] OpenCode Go 账户时间线回填失败：{error}（下次启动重试）"),
    }
    let mut first = true;
    while !STOPPED.load(Ordering::Relaxed) {
        let started = std::time::Instant::now();
        let now = sources::now_ms();
        match collector.run_round(&mut connection, now) {
            Ok(report) => {
                let total: i64 = connection
                    .query_row("SELECT COUNT(*) FROM usage_event", [], |row| row.get(0))
                    .unwrap_or(-1);
                println!(
                    "round ok：新增 {} 条，usage_event 共 {} 条，用时 {:?}{}",
                    report.inserted,
                    total,
                    started.elapsed(),
                    if first {
                        "（首轮仅记录水位，不回填）"
                    } else {
                        ""
                    }
                );
                if report.inserted > 0 {
                    if let Ok(unpriced) = connection.query_row(
                        "SELECT COUNT(*) FROM usage_event WHERE cost_usd IS NULL",
                        [],
                        |row| row.get::<_, i64>(0),
                    ) {
                        if unpriced > 0 {
                            println!("  未计价事件累计 {unpriced} 条（见 unresolved_model）");
                        }
                    }
                }
            }
            Err(error) => eprintln!("round 失败：{error}"),
        }
        first = false;
        sleep_until_next_round(sources::ROUND);
    }
    println!("herdr-usage 退出");
}

/// Run the one-off `provider` backfill unless its marker says it is done.
///
/// Returns `None` when the marker is already present, otherwise the number of
/// rows filled. The marker is written only after a successful pass, so an
/// interrupted backfill runs again on the next start (the update itself is
/// idempotent: it only touches rows that are still NULL).
fn backfill_once(
    connection: &mut rusqlite::Connection,
    roots: &[std::path::PathBuf],
) -> rusqlite::Result<Option<usize>> {
    if db::offset(connection, BACKFILL_KIND)?.is_some() {
        return Ok(None);
    }
    let rows = sources::backfill_omp_provider(connection, roots)?;
    db::save_offset(connection, BACKFILL_KIND, "1")?;
    Ok(Some(rows))
}

fn request_backfill_once(
    connection: &mut rusqlite::Connection,
    roots: &[std::path::PathBuf],
) -> rusqlite::Result<Option<usize>> {
    if db::offset(connection, REQUEST_BACKFILL_KIND)?.is_some() {
        return Ok(None);
    }
    let rows = sources::backfill_omp_request_metadata(connection, roots)?;
    db::save_offset(connection, REQUEST_BACKFILL_KIND, "1")?;
    Ok(Some(rows))
}

fn api_key_timeline_backfill_once(
    connection: &mut rusqlite::Connection,
    roots: &[std::path::PathBuf],
) -> rusqlite::Result<Option<usize>> {
    if db::offset(connection, API_KEY_TIMELINE_BACKFILL_KIND)?.is_some() {
        return Ok(None);
    }
    let rows = sources::backfill_omp_api_key_timeline(connection, roots)?;
    db::save_offset(connection, API_KEY_TIMELINE_BACKFILL_KIND, "1")?;
    Ok(Some(rows))
}

/// Sleep in short slices so a signal is honoured promptly.
fn sleep_until_next_round(duration: Duration) {
    let step = Duration::from_millis(500);
    let mut slept = Duration::ZERO;
    while slept < duration && !STOPPED.load(Ordering::Relaxed) {
        std::thread::sleep(step.min(duration - slept));
        slept += step;
    }
}

/// Flip a flag on SIGINT/SIGTERM so the current round finishes and the process
/// exits cleanly. A trait object would be unsafe here (only async-signal-safe
/// work is allowed in a handler), so the flag is a process-global atomic.
fn install_signal_handlers() {
    extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }
    extern "C" fn on_signal(_signal: i32) {
        STOPPED.store(true, Ordering::Relaxed);
    }
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        signal(2, on_signal as *const () as usize); // SIGINT
        signal(15, on_signal as *const () as usize); // SIGTERM
    }
}
