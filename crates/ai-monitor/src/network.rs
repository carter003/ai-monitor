//! Unauthenticated HTTPS entry-point probes. HTTP status is not model health.
use crate::worker::Update;
use std::{
    collections::VecDeque,
    io::Read,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const ROUTES: [(&str, &str); 4] = [
    ("Codex", "https://chatgpt.com/backend-api/codex/responses"),
    (
        "OpenCode Go",
        "https://opencode.ai/zen/go/v1/chat/completions",
    ),
    (
        "SuperGrok",
        "https://cli-chat-proxy.grok.com/v1/chat/completions",
    ),
    (
        "Antigravity",
        "https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent",
    ),
];
const WINDOW: Duration = Duration::from_secs(300);
const MIN_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub struct Probe {
    pub connect: Option<Duration>,
    pub response: Option<Duration>,
    pub status: Option<u16>,
    pub note: Option<String>,
    pub retry_after: Duration,
}
impl Probe {
    fn failed(note: &str) -> Self {
        Self {
            connect: None,
            response: None,
            status: None,
            note: Some(note.into()),
            retry_after: Duration::ZERO,
        }
    }
}
pub struct NetworkUpdate {
    pub route: usize,
    pub at: Instant,
    pub probe: Probe,
}
#[derive(Default)]
pub struct RouteState {
    pub current: Option<Probe>,
    samples: VecDeque<(Instant, bool)>,
}
impl RouteState {
    pub fn rate(&self, now: Instant) -> Option<f64> {
        let samples: Vec<_> = self
            .samples
            .iter()
            .filter(|(at, _)| now.saturating_duration_since(*at) < WINDOW)
            .collect();
        (samples.len() >= 3).then(|| {
            100. * samples.iter().filter(|(_, ok)| *ok).count() as f64 / samples.len() as f64
        })
    }
    fn finish(&mut self, probe: Probe, at: Instant) {
        while self
            .samples
            .front()
            .is_some_and(|(t, _)| at.saturating_duration_since(*t) >= WINDOW)
        {
            self.samples.pop_front();
        }
        self.samples.push_back((at, probe.status.is_some()));
        self.current = Some(probe);
    }
}
pub struct NetworkState {
    pub enabled: bool,
    pub interval: Duration,
    pub routes: [RouteState; 4],
}
impl NetworkState {
    pub fn new(enabled: bool, interval: Duration) -> Self {
        Self {
            enabled,
            interval,
            routes: std::array::from_fn(|_| RouteState::default()),
        }
    }
    pub fn apply(&mut self, update: NetworkUpdate) {
        self.routes[update.route].finish(update.probe, update.at);
    }
}

fn retry_after(value: &str, now: i64) -> Duration {
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Duration::from_secs(seconds);
    }
    chrono::DateTime::parse_from_rfc2822(value.trim())
        .ok()
        .map(|date| Duration::from_secs(date.timestamp().saturating_sub(now).max(0) as u64))
        .unwrap_or_default()
}

fn parse(output: &str, code: Option<i32>, now: i64) -> Probe {
    let Some((headers, timing)) = output.rsplit_once("AI_MONITOR_TIMING ") else {
        return Probe::failed("探测异常");
    };
    let fields: Vec<_> = timing.split_whitespace().collect();
    let duration = |index: usize| {
        fields
            .get(index)
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0. && *v <= 6.)
            .map(Duration::from_secs_f64)
    };
    let connect = duration(1);
    let response = duration(2);
    let status = fields
        .first()
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|s| (100..=599).contains(s));
    if let (Some(status), Some(connect), Some(response)) = (status, connect, response) {
        // Only the last header block belongs to the endpoint, not proxy CONNECT.
        let normalized = headers.replace("\r\n", "\n");
        let last = normalized.trim().rsplit("\n\n").next().unwrap_or("");
        let retry = if status == 429 {
            last.lines()
                .filter_map(|line| line.split_once(':'))
                .filter(|(key, _)| key.eq_ignore_ascii_case("retry-after"))
                .map(|(_, value)| retry_after(value, now))
                .max()
                .unwrap_or_default()
        } else {
            Duration::ZERO
        };
        return Probe {
            connect: Some(connect),
            response: Some(response),
            status: Some(status),
            note: (status == 403 || status == 429 || status >= 500)
                .then(|| format!("HTTP {status}")),
            retry_after: retry,
        };
    }
    Probe::failed(match code {
        Some(5 | 6) => "DNS失败",
        Some(7) => "连接失败",
        Some(28) => "超时",
        Some(35 | 51 | 58 | 60 | 77 | 83 | 90 | 91) => "TLS失败",
        _ => "连接失败",
    })
}

/// A fresh process means no reused connection. Never reads service credentials.
fn command(url: &str) -> Command {
    let mut command = Command::new("curl");
    command.args([
        "--disable",
        "--silent",
        "--head",
        "--max-time",
        "5",
        "--connect-timeout",
        "5",
        "--proto",
        "=https",
        "--suppress-connect-headers",
        "--dump-header",
        "-",
        "--output",
        "/dev/null",
        "--write-out",
        "\nAI_MONITOR_TIMING %{http_code} %{time_appconnect} %{time_starttransfer}\n",
        "--url",
        url,
    ]);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        let parent = std::process::id() as libc::pid_t;
        // Covers watchdog process::exit and SIGKILL, which do not run Drop.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
    }
    command
}

fn execute(mut command: Command, stop: &AtomicBool, timeout: Duration) -> Option<Probe> {
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return Some(Probe::failed(if e.kind() == std::io::ErrorKind::NotFound {
                "缺少curl"
            } else {
                "无法启动curl"
            }));
        }
    };
    let mut stdout = child.stdout.take().expect("piped stdout");
    // Drain continuously, retaining only a bounded tail (headers can be large).
    let reader = thread::spawn(move || {
        let mut output = Vec::new();
        let mut chunk = [0; 4096];
        while let Ok(n) = stdout.read(&mut chunk) {
            if n == 0 {
                break;
            }
            output.extend_from_slice(&chunk[..n]);
            if output.len() > 65536 {
                output.drain(..output.len() - 65536);
            }
        }
        String::from_utf8_lossy(&output).into_owned()
    });
    let started = Instant::now();
    let (code, timed_out) = loop {
        if stop.load(Ordering::Relaxed) || started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            break (None, true);
        }
        match child.try_wait() {
            Ok(Some(status)) => break (status.code(), false),
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break (None, false);
            }
        }
    };
    let output = reader.join().unwrap_or_default();
    if stop.load(Ordering::Relaxed) {
        return None;
    }
    Some(if timed_out {
        Probe::failed("超时")
    } else {
        parse(&output, code, chrono::Utc::now().timestamp())
    })
}

fn wait_for_next(
    started: Instant,
    finished: Instant,
    interval: Duration,
    backoff: Duration,
    rx: &Receiver<()>,
    stop: &AtomicBool,
) -> bool {
    let mut pending = false;
    loop {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let cadence = if pending {
            MIN_INTERVAL
        } else {
            interval.max(MIN_INTERVAL)
        };
        // Relative durations avoid Instant overflow even for a maximal Retry-After.
        let remaining = cadence
            .saturating_sub(started.elapsed())
            .max(backoff.saturating_sub(finished.elapsed()));
        if remaining.is_zero() {
            return true;
        }
        match rx.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(()) => pending = true,
            Err(mpsc::RecvTimeoutError::Disconnected) => return false,
            _ => (),
        }
    }
}

fn run_with(
    route: usize,
    interval: Duration,
    tx: Sender<Update>,
    rx: Receiver<()>,
    stop: Arc<AtomicBool>,
    mut probe: impl FnMut() -> Option<Probe>,
) {
    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        let Some(probe) = probe() else {
            return;
        };
        let at = Instant::now();
        let backoff = probe.retry_after;
        // Refreshes received during the request are satisfied by that request.
        while rx.try_recv().is_ok() {}
        if tx
            .send(Update::Network(NetworkUpdate { route, at, probe }))
            .is_err()
        {
            return;
        }
        if !wait_for_next(started, at, interval, backoff, &rx, &stop) {
            return;
        }
    }
}

pub fn start(
    interval: Duration,
    tx: Sender<Update>,
    stop: Arc<AtomicBool>,
) -> (Vec<SyncSender<()>>, Vec<JoinHandle<()>>) {
    let mut senders = vec![];
    let mut handles = vec![];
    for (route, (_, url)) in ROUTES.iter().enumerate() {
        let (trigger, rx) = mpsc::sync_channel(1);
        senders.push(trigger);
        let (tx, stop) = (tx.clone(), stop.clone());
        handles.push(thread::spawn(move || {
            let flag = stop.clone();
            run_with(route, interval, tx, rx, stop, || {
                execute(command(url), &flag, MIN_INTERVAL)
            })
        }));
    }
    (senders, handles)
}

#[cfg(test)]
mod tests;
