use ai_monitor::{
    config::Config,
    model::{Source, SourceState, UsageStats},
    system::SystemSampler,
    ui::{self, View},
    worker::{Update, Workers},
};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::{
    io::{self, IsTerminal},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

/// How often the hangup watchdog probes the terminal. One `TIOCGWINSZ` per
/// second costs nothing; the interval bounds how long a dead terminal keeps
/// the process alive (see `watch_terminal`).
const HANGUP_PROBE_SLICE: Duration = Duration::from_millis(50);
const HANGUP_PROBE_SLICES: usize = 20;

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Set by the main loop once it has left its event loop, immediately before it
/// joins the watchdog.
///
/// The watchdog must stop on this flag and *not* on `SHUTDOWN`: the kernel
/// delivers `SIGHUP` when the pty is hung up, the handler sets `SHUTDOWN`, and
/// the main loop is stuck in crossterm at that very moment. Stopping on
/// `SHUTDOWN` would retire the watchdog at the one instant its probe is the
/// only thing that can still end the process.
static FINISHED: AtomicBool = AtomicBool::new(false);

fn install_signal_handlers() {
    extern "C" fn on_signal(_: libc::c_int) {
        SHUTDOWN.store(true, Ordering::Relaxed);
    }
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as usize);
        libc::signal(libc::SIGHUP, on_signal as *const () as usize);
        libc::signal(libc::SIGINT, on_signal as *const () as usize);
    }
}

/// True while the controlling terminal can still be queried. A pty hangup
/// makes `open("/dev/tty")` fail with `ENXIO`, and a terminal that is going
/// away makes `TIOCGWINSZ` fail with `EIO`; either way this returns `false`, so
/// the UI exits instead of spinning in `event::poll` on a terminal that will
/// never deliver input again. Measured on Linux: a hung-up pty slave reports
/// `POLLIN|POLLERR|POLLHUP` to pollers and `EIO` from `TIOCGWINSZ`.
fn terminal_alive() -> bool {
    // SAFETY: both calls take pointers to local memory or plain ints and have
    // no preconditions beyond validity of the fd, which `open` just returned.
    let fd = unsafe { libc::open(c"/dev/tty".as_ptr(), libc::O_RDONLY) };
    if fd < 0 {
        return false;
    }
    let mut winsize = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `winsize` is a plain-old-data struct valid for the call.
    let ok = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut winsize) } == 0;
    // SAFETY: `fd` came from the successful `open` above.
    unsafe { libc::close(fd) };
    ok
}

/// Watch the terminal in the background and end the process when it hangs up.
///
/// This exists because crossterm's event source spins on a hung-up pty: the
/// poll reports the tty permanently readable and every read returns EOF, so
/// the `event::poll` call never returns and the main loop can no longer
/// re-check the shutdown flag - a signal handler alone cannot rescue it. The
/// watchdog therefore notices the hangup within one probe interval, restores
/// the terminal, and exits the process; the spinning main thread dies with it.
///
/// It stops only on `FINISHED`. The hangup arrives together with `SIGHUP`, so
/// keying the stop on `SHUTDOWN` would retire the watchdog microseconds before
/// its probe - while the main loop is stuck and cannot act on that flag either.
fn watch_terminal() -> thread::JoinHandle<()> {
    thread::spawn(move || {
        loop {
            if !terminal_alive() {
                // The main loop may be stuck inside crossterm, so it cannot run
                // TerminalGuard's drop. Restore the terminal here, then end the
                // process unconditionally.
                let _ = terminal::disable_raw_mode();
                let _ = execute!(
                    io::stdout(),
                    LeaveAlternateScreen,
                    crossterm::event::DisableMouseCapture,
                    crossterm::cursor::Show
                );
                std::process::exit(0);
            }
            if FINISHED.load(Ordering::Relaxed) {
                return;
            }
            for _ in 0..HANGUP_PROBE_SLICES {
                if FINISHED.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(HANGUP_PROBE_SLICE);
            }
        }
    })
}

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture,
            crossterm::cursor::Show
        );
    }
}

/// Apply one action, whether it came from a key or a mouse gesture.
fn apply(view: &mut View, workers: &Workers, action: ui::FooterAction) {
    match action {
        ui::FooterAction::Refresh => workers.refresh(),
        ui::FooterAction::ScrollUp => view.scroll = view.scroll.saturating_sub(1),
        ui::FooterAction::ScrollDown => {
            view.scroll = view.scroll.saturating_add(1).min(view.max_scroll)
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("ai-monitor: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args_os().len() > 1 {
        return Err("直接运行 ai-monitor 即可，无需参数".into());
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("请在交互终端中运行 ai-monitor".into());
    }
    let config = Config::load()?;
    install_signal_handlers();
    // Must start after the terminal is known-good; it runs for the whole
    // session and exits the UI when the pty hangs up (see `watch_terminal`).
    let hangup_watch = watch_terminal();
    let old_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture,
            crossterm::cursor::Show
        );
        old_hook(info);
    }));
    terminal::enable_raw_mode()?;
    let _guard = TerminalGuard;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let refresh = config.refresh;
    let workers = Workers::start(config);
    let mut states: Vec<_> = Source::ALL
        .into_iter()
        .map(|source| SourceState::new(source, refresh))
        .collect();
    let mut sampler = SystemSampler::new();
    sampler.sample();
    let mut usage = UsageStats::default();
    let mut view = View::default();
    let mut tick = Instant::now();
    let mut dirty = true;
    while !SHUTDOWN.load(Ordering::Relaxed) {
        for update in workers.updates.try_iter() {
            match update {
                Update::Started(source, identity) => {
                    if let Some(state) = states.iter_mut().find(|s| s.source == source) {
                        state.begin(identity);
                    }
                }
                Update::Finished(source, result, at, next) => {
                    if let Some(state) = states.iter_mut().find(|s| s.source == source) {
                        state.finish(result, at, next);
                    }
                }
                Update::Usage(stats) => usage = *stats,
            }
            dirty = true;
        }
        if tick.elapsed() >= Duration::from_secs(1) {
            sampler.sample();
            tick = Instant::now();
            dirty = true;
        }
        if dirty {
            terminal.draw(|f| {
                ui::draw(
                    f,
                    &sampler.stats,
                    &states,
                    &usage,
                    &mut view,
                    chrono::Utc::now().timestamp(),
                )
            })?;
            dirty = false;
        }
        // After a hangup, crossterm can surface EIO from its poll/read path.
        // Treat every poll error as a terminal teardown signal: the UI cannot
        // recover input once the pty is gone, and retrying would busy-loop.
        let polled = match event::poll(Duration::from_millis(100)) {
            Ok(polled) => polled,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if polled {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            break;
                        }
                        KeyCode::Char('r') => apply(&mut view, &workers, ui::FooterAction::Refresh),
                        KeyCode::Up | KeyCode::Char('k') => {
                            apply(&mut view, &workers, ui::FooterAction::ScrollUp)
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            apply(&mut view, &workers, ui::FooterAction::ScrollDown)
                        }
                        KeyCode::PageUp => {
                            view.scroll = view.scroll.saturating_sub(view.page_size.max(1))
                        }
                        KeyCode::PageDown => {
                            view.scroll = view
                                .scroll
                                .saturating_add(view.page_size.max(1))
                                .min(view.max_scroll)
                        }
                        KeyCode::Home => view.scroll = 0,
                        KeyCode::End => view.scroll = view.max_scroll,
                        _ => (),
                    }
                    dirty = true;
                }
                // Mouse gestures reuse the same actions as the keys, so the two
                // paths cannot diverge. A left click acts only on a footer hint
                // whose region `draw` recorded; the wheel scrolls the body.
                Event::Mouse(mouse) => {
                    let in_body = view.body.is_some_and(|body| {
                        mouse.column >= body.x
                            && mouse.column < body.right()
                            && mouse.row >= body.y
                            && mouse.row < body.bottom()
                    });
                    let action = match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left)
                            if view.footer_row == Some(mouse.row) =>
                        {
                            view.footer_hits
                                .iter()
                                .find(|(first, last, _)| {
                                    mouse.column >= *first && mouse.column <= *last
                                })
                                .map(|(_, _, action)| *action)
                        }
                        MouseEventKind::Down(MouseButton::Middle) => {
                            Some(ui::FooterAction::Refresh)
                        }
                        MouseEventKind::ScrollUp if in_body => Some(ui::FooterAction::ScrollUp),
                        MouseEventKind::ScrollDown if in_body => Some(ui::FooterAction::ScrollDown),
                        _ => None,
                    };
                    if let Some(action) = action {
                        apply(&mut view, &workers, action);
                        dirty = true;
                    }
                }
                Event::Resize(_, _) => dirty = true,
                _ => (),
            }
        }
    }
    // Let the watchdog stop before tearing down the terminal it probes. Only
    // `FINISHED` retires it: a `SHUTDOWN` set by a signal must leave the probe
    // armed, since that signal is also how a pty hangup announces itself.
    FINISHED.store(true, Ordering::Relaxed);
    let _ = hangup_watch.join();
    Ok(())
}

#[cfg(test)]
mod hangup_tests {
    use super::*;

    /// Without a controlling terminal `open("/dev/tty")` fails, which must
    /// read as "terminal gone" - the same answer the watchdog needs after a
    /// pty hangup. Cargo's test harness has no tty, so this exercises the
    /// failure path deterministically.
    #[test]
    fn a_missing_controlling_terminal_reports_dead() {
        assert!(!terminal_alive());
    }
}
