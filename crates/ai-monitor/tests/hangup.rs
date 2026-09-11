//! A closed terminal must end the UI, and `q` must still end it too.
//!
//! A pty hangup also delivers `SIGHUP`, whose handler sets the shutdown flag
//! that the main loop is stuck too deep inside crossterm to read. If the
//! watchdog keyed its own retirement on that flag it would stop probing at the
//! one moment its probe is the only thing that can still end the process - and
//! the process would spin forever on a terminal nobody is attached to any more.
//!
//! These tests reproduce the real thing with tmux, because a pty this test
//! opens itself does not behave the same way: closing the master from the test
//! process makes the UI leave through crossterm's error path without ever
//! needing the watchdog, so such a test passes with or without the fix. tmux is
//! how the leak was reported, so tmux is what the regression is pinned to. The
//! tests skip when tmux is not installed.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

/// How long the UI may take to disappear. The watchdog probes once a second, so
/// this tolerates a slow machine without hiding a process that never leaves.
const EXIT_DEADLINE: Duration = Duration::from_secs(10);

/// Let the UI reach its event loop before the test acts on it.
const STARTUP: Duration = Duration::from_secs(3);

fn tmux(socket: &str, args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .arg("-L")
        .arg(socket)
        .arg("-f")
        .arg("/dev/null")
        .args(args)
        .output()
        .expect("run tmux")
}

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// The pid of the UI in the session's only pane. tmux starts a pane as
/// `sh -c <command>` and the shell execs the UI, so this is the UI's own pid;
/// the process is its own session and group leader (`sid == pgid == pid`).
fn pane_pid(socket: &str) -> Option<i32> {
    let output = tmux(socket, &["list-panes", "-t", "ui", "-F", "#{pane_pid}"]);
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

/// Whether `pid` is still our binary. Checking `/proc/<pid>/exe` rather than
/// signalling alone keeps a recycled pid from reading as a surviving UI.
fn ui_alive(pid: i32, binary: &Path) -> bool {
    fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|exe| exe == binary)
}

/// Wait for the UI to disappear, reporting how long it took.
fn wait_gone(pid: i32, binary: &Path) -> Result<Duration, ()> {
    let started = Instant::now();
    while started.elapsed() < EXIT_DEADLINE {
        if !ui_alive(pid, binary) {
            return Ok(started.elapsed());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(())
}

/// An isolated config so no test reads real credentials or reaches the network.
fn isolated_config(dir: &Path) -> PathBuf {
    let config = dir.join("config.toml");
    fs::write(
        &config,
        format!(
            "codex_home = \"{}\"\n\
             agy_home = \"{}\"\n\
             agy2_home = \"{}\"\n\
             opencode_home = \"{}\"\n\
             grok_home = \"{}\"\n\
             openrouter_key_file = \"{}\"\n\
             usage_db = \"{}\"\n",
            dir.join("codex").display(),
            dir.join("gemini").display(),
            dir.join("gemini2").display(),
            dir.join("opencode").display(),
            dir.join("grok").display(),
            dir.join("openrouter.key").display(),
            dir.join("usage.db").display(),
        ),
    )
    .expect("write isolated config");
    config
}

/// Start the UI in a detached tmux session and return its pid.
fn start_ui(socket: &str, config: &Path, home: &Path, binary: &Path) -> i32 {
    tmux(socket, &["kill-server"]);
    let output = Command::new("tmux")
        .arg("-L")
        .arg(socket)
        .arg("-f")
        .arg("/dev/null")
        .args([
            "new-session",
            "-d",
            "-s",
            "ui",
            "-x",
            "100",
            "-y",
            "30",
            binary.to_str().expect("utf-8 binary path"),
        ])
        .env("AI_MONITOR_CONFIG", config)
        .env("HOME", home)
        .env_remove("CODEX_HOME")
        .env_remove("GROK_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("OPENROUTER_MANAGEMENT_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .output()
        .expect("start tmux session");
    assert!(
        output.status.success(),
        "tmux could not start the UI: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    thread::sleep(STARTUP);
    let pid = pane_pid(socket).expect("the UI pane has a pid");
    assert!(
        ui_alive(pid, binary),
        "the UI was not running as {binary:?} when the test was ready"
    );
    pid
}

/// End the UI's whole process group; the test is not its parent, and a stuck
/// process ignores a polite signal.
fn kill_ui(pid: i32) {
    // SAFETY: `pid` names the test's own UI process, verified through /proc.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
        libc::kill(pid, libc::SIGKILL);
    }
}

fn teardown(socket: &str) {
    tmux(socket, &["kill-server"]);
}

/// True when tmux is present; otherwise announce the skip so a silent pass is
/// not mistaken for coverage.
fn tmux_or_skip() -> bool {
    if tmux_available() {
        true
    } else {
        eprintln!("skipping: tmux is not installed");
        false
    }
}

#[test]
fn killing_the_terminal_ends_the_ui() {
    if !tmux_or_skip() {
        return;
    }
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_ai-monitor"));
    let Some(dir) = tempfile::tempdir().ok() else {
        eprintln!("skipping: no temporary directory");
        return;
    };
    let socket = "ai-monitor-test-hangup";
    let pid = start_ui(socket, &isolated_config(dir.path()), dir.path(), &binary);

    // What closing a terminal window or a tmux session does to the pane.
    tmux(socket, &["kill-session", "-t", "ui"]);
    match wait_gone(pid, &binary) {
        Ok(elapsed) => println!("exited {elapsed:?} after the terminal went away"),
        Err(()) => {
            kill_ui(pid);
            teardown(socket);
            panic!("the UI survived the pty hangup and is still running as pid {pid}");
        }
    }
    teardown(socket);
}

#[test]
fn pressing_q_ends_the_ui() {
    if !tmux_or_skip() {
        return;
    }
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_ai-monitor"));
    let Some(dir) = tempfile::tempdir().ok() else {
        eprintln!("skipping: no temporary directory");
        return;
    };
    let socket = "ai-monitor-test-quit";
    let pid = start_ui(socket, &isolated_config(dir.path()), dir.path(), &binary);

    // The quit key must retire the watchdog too, or the final join hangs.
    tmux(socket, &["send-keys", "-t", "ui", "q"]);
    match wait_gone(pid, &binary) {
        Ok(elapsed) => println!("exited {elapsed:?} after `q`"),
        Err(()) => {
            kill_ui(pid);
            teardown(socket);
            panic!("the UI ignored `q` and is still running as pid {pid}");
        }
    }
    teardown(socket);
}
