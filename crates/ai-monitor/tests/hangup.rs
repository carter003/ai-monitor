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
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
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
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    fs::write(dir.join("web-port"), port.to_string()).unwrap();
    drop(listener);
    fs::write(
        &config,
        format!(
            "network_enabled = false\n\
             web_port = {port}\n\
             codex_home = \"{}\"\n\
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
    let database = herdr_usage::db::open(&dir.join("usage.db")).unwrap();
    database.execute("INSERT INTO usage_event(source, event_id, model, model_source, input_total, cache_read, cache_write, output_total, reasoning, cost_usd, occurred_at) VALUES ('codex','test','test-model','event',100,0,0,20,0,1.25,0)", []).unwrap();
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
        .env(
            "PATH",
            format!(
                "{}:{}",
                home.join("bin").display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
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
    let address = web_address(home);
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .write_all(b"GET /api/usage HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "web not ready: {response}"
    );
    let (_, body) = response.split_once("\r\n\r\n").unwrap();
    let report: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(
        report["overview"]["all"]["tokens"], 120,
        "web must use the TUI config database"
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
    assert_web_closed(dir.path());
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
    assert_web_closed(dir.path());
    teardown(socket);
}

fn web_address(dir: &Path) -> SocketAddr {
    format!(
        "127.0.0.1:{}",
        fs::read_to_string(dir.join("web-port")).unwrap()
    )
    .parse()
    .unwrap()
}

fn assert_web_closed(dir: &Path) {
    let address = web_address(dir);
    // SIGKILL can remove /proc/<pid>/exe before the last worker has completed
    // kernel socket teardown. Check the resource itself within a bounded wait.
    let started = Instant::now();
    while TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "web survived UI exit"
        );
        thread::sleep(Duration::from_millis(25));
    }
    let _rebound = TcpListener::bind(address).expect("web listener must release its port");
}

#[test]
fn signals_and_quit_keys_close_the_web_even_with_an_unfinished_request() {
    if !tmux_or_skip() {
        return;
    }
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_ai-monitor"));
    for action in ["C-c", "Escape", "SIGTERM", "SIGHUP", "SIGKILL"] {
        let dir = tempfile::tempdir().unwrap();
        let socket = format!("ai-monitor-test-web-{action}");
        let pid = start_ui(&socket, &isolated_config(dir.path()), dir.path(), &binary);
        let mut unfinished = TcpStream::connect(web_address(dir.path())).unwrap();
        unfinished.write_all(b"GET / HTTP/1.1\r\nHost:").unwrap();
        match action {
            "C-c" | "Escape" => {
                tmux(&socket, &["send-keys", "-t", "ui", action]);
            }
            signal => {
                let number = match signal {
                    "SIGTERM" => libc::SIGTERM,
                    "SIGHUP" => libc::SIGHUP,
                    _ => libc::SIGKILL,
                };
                // SAFETY: pid is the test's live UI process, verified by start_ui.
                unsafe {
                    libc::kill(pid, number);
                }
            }
        }
        if wait_gone(pid, &binary).is_err() {
            kill_ui(pid);
            teardown(&socket);
            panic!("UI survived {action}");
        }
        assert_web_closed(dir.path());
        println!("{action}: UI exited and web port released");
        teardown(&socket);
    }
}

#[test]
fn unfinished_network_children_end_with_ui() {
    use std::os::unix::fs::PermissionsExt;
    if !tmux_or_skip() {
        return;
    }
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_ai-monitor"));
    for action in ["q", "SIGKILL", "hangup"] {
        let dir = tempfile::tempdir().unwrap();
        let config = isolated_config(dir.path());
        fs::write(
            &config,
            fs::read_to_string(&config)
                .unwrap()
                .replace("network_enabled = false", "network_enabled = true"),
        )
        .unwrap();
        fs::create_dir(dir.path().join("bin")).unwrap();
        let curl = dir.path().join("bin/curl");
        // An exec keeps the fake curl as a single child, just like real curl.
        fs::write(
            &curl,
            "#!/bin/sh\necho $$ >> \"$HOME/probe-pids\"\nexec /bin/sleep 30\n",
        )
        .unwrap();
        fs::set_permissions(&curl, fs::Permissions::from_mode(0o755)).unwrap();
        let socket = format!("ai-monitor-test-network-{action}");
        let pid = start_ui(&socket, &config, dir.path(), &binary);
        let pids: Vec<u32> = fs::read_to_string(dir.path().join("probe-pids"))
            .expect("curl started immediately")
            .lines()
            .map(|s| s.parse().unwrap())
            .collect();
        assert_eq!(pids.len(), 4, "exactly one in-flight probe per route");
        match action {
            "q" => {
                tmux(&socket, &["send-keys", "-t", "ui", "q"]);
            }
            "hangup" => {
                tmux(&socket, &["kill-session", "-t", "ui"]);
            }
            _ => unsafe {
                libc::kill(pid, libc::SIGKILL);
            },
        }
        if wait_gone(pid, &binary).is_err() {
            kill_ui(pid);
            teardown(&socket);
            panic!("UI survived {action}");
        }
        for child in pids {
            let started = Instant::now();
            while fs::read_link(format!("/proc/{child}/exe")).is_ok() {
                assert!(
                    started.elapsed() < Duration::from_secs(2),
                    "probe {child} survived {action}"
                );
                thread::sleep(Duration::from_millis(20));
            }
            if action == "q" {
                assert!(
                    !Path::new(&format!("/proc/{child}")).exists(),
                    "normal exit must reap child"
                );
            }
        }
        teardown(&socket);
    }
}
