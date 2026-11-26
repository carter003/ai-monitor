//! Run a dashboard probe with bounded output and lifetime.
use std::{
    io::{self, Read},
    os::unix::process::CommandExt,
    process::{Command, ExitStatus, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

pub(crate) fn output(
    mut command: Command,
    stop: &AtomicBool,
    timeout: Duration,
) -> io::Result<(ExitStatus, String)> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    #[cfg(target_os = "linux")]
    {
        let parent = std::process::id() as libc::pid_t;
        // Also cover abrupt dashboard exit when Drop cannot run.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
    }
    let mut child = command.spawn()?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = thread::spawn(move || {
        let mut output = Vec::new();
        let mut chunk = [0; 4096];
        loop {
            let n = stdout.read(&mut chunk)?;
            if n == 0 {
                return Ok::<_, io::Error>(String::from_utf8_lossy(&output).into_owned());
            }
            output.extend_from_slice(&chunk[..n]);
            if output.len() > 65536 {
                output.drain(..output.len() - 65536);
            }
        }
    });
    let started = Instant::now();
    let status = loop {
        if stop.load(Ordering::Relaxed) {
            break Err(io::ErrorKind::Interrupted.into());
        }
        if started.elapsed() >= timeout {
            break Err(io::ErrorKind::TimedOut.into());
        }
        // A CLI helper can still hold the stdout pipe after its parent exits.
        // Keep the deadline active and the process group leader unreaped until EOF.
        if !reader.is_finished() {
            thread::sleep(Duration::from_millis(20));
            continue;
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(error) => break Err(error),
        }
    };
    let status = status.inspect_err(|_| {
        // The child leads its own group, including any CLI helper processes.
        unsafe {
            libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
        }
        let _ = child.wait();
    })?;
    let body = reader
        .join()
        .map_err(|_| io::Error::other("stdout reader stopped"))?;
    Ok((status, body?))
}
