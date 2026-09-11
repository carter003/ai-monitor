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

    let mut collector = sources::Collector::new(paths);
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
