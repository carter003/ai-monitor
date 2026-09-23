use super::*;
fn reply(status: u16) -> Probe {
    parse(
        &format!(
            "HTTP/2 {status}\r\nRetry-After: 12\r\n\r\nAI_MONITOR_TIMING {status} 0.22 0.58\n"
        ),
        Some(0),
        0,
    )
}
#[test]
fn http_errors_are_reachable_and_only_429_backs_off() {
    for status in [200, 301, 401, 403, 404, 405, 429, 500, 503] {
        let p = reply(status);
        assert_eq!(p.status, Some(status));
        assert_eq!(p.connect, Some(Duration::from_millis(220)));
        assert_eq!(p.response, Some(Duration::from_millis(580)));
        assert_eq!(p.retry_after.as_secs(), if status == 429 { 12 } else { 0 });
        assert_eq!(
            p.note.is_some(),
            status == 403 || status == 429 || status >= 500
        );
    }
}
#[test]
fn failures_clear_timings_and_proxy_response_does_not_count() {
    for (code, note) in [
        (6, "DNS失败"),
        (7, "连接失败"),
        (28, "超时"),
        (60, "TLS失败"),
    ] {
        let p = parse(
            "HTTP/1.1 200 Connection established\r\n\r\nAI_MONITOR_TIMING 000 0 0\n",
            Some(code),
            0,
        );
        assert!(p.status.is_none());
        assert!(p.connect.is_none());
        assert!(p.response.is_none());
        assert_eq!(p.note.as_deref(), Some(note));
    }
    for timing in ["200 NaN 1", "200 0.1 inf", "200 -1 1", "200 0.1"] {
        assert!(
            parse(&format!("AI_MONITOR_TIMING {timing}"), Some(0), 0)
                .status
                .is_none()
        );
    }
}
#[test]
fn rolling_window_sampling_and_failure_replace_current() {
    let now = Instant::now();
    let mut state = RouteState::default();
    assert_eq!(state.rate(now), None);
    state.finish(reply(401), now);
    state.finish(reply(405), now + Duration::from_secs(10));
    assert_eq!(state.rate(now + Duration::from_secs(10)), None);
    state.finish(Probe::failed("超时"), now + Duration::from_secs(20));
    assert!((state.rate(now + Duration::from_secs(20)).unwrap() - 200. / 3.).abs() < 0.001);
    assert!(state.current.as_ref().unwrap().connect.is_none());
    assert_eq!(state.rate(now + WINDOW), None);
    state.finish(reply(200), now + WINDOW + Duration::from_secs(20));
    assert_eq!(state.samples.len(), 1);
}
#[test]
fn retry_after_date_and_final_headers() {
    assert_eq!(
        retry_after("Thu, 01 Jan 1970 00:01:00 GMT", 10).as_secs(),
        50
    );
    assert_eq!(
        retry_after("Thu, 01 Jan 1970 00:01:00 GMT", 100).as_secs(),
        0
    );
    assert_eq!(retry_after("bad", 0), Duration::ZERO);
    let p = parse(
        "HTTP/1.1 200 Connection established\r\nRetry-After: 900\r\n\r\nHTTP/2 429\r\nretry-after: 17\r\n\r\nAI_MONITOR_TIMING 429 0.1 0.2",
        Some(0),
        0,
    );
    assert_eq!(p.retry_after.as_secs(), 17);
}
#[test]
fn cancellation_and_timeout_reap_child() {
    for cancel in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "echo $$ > \"$1\"; exec sleep 30", "test"])
            .arg(&pid_file)
            .stdout(Stdio::piped());
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = thread::spawn(move || execute(cmd, &flag, Duration::from_millis(300)));
        let start = Instant::now();
        while !pid_file.exists() && start.elapsed() < Duration::from_secs(2) {
            thread::sleep(Duration::from_millis(10));
        }
        if cancel {
            stop.store(true, Ordering::Relaxed);
        }
        let result = handle.join().unwrap();
        if cancel {
            assert!(result.is_none());
        } else {
            assert_eq!(result.unwrap().note.as_deref(), Some("超时"));
        }
        let pid = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
#[test]
fn missing_curl_is_an_explanatory_failure() {
    let p = execute(
        Command::new("/nonexistent/ai-monitor-curl"),
        &AtomicBool::new(false),
        MIN_INTERVAL,
    )
    .unwrap();
    assert_eq!(p.note.as_deref(), Some("缺少curl"));
}
#[test]
#[ignore = "real network: four unauthenticated HEAD requests"]
fn live_routes() {
    let handles: Vec<_> = ROUTES
        .iter()
        .map(|(name, url)| {
            thread::spawn(move || {
                let probe = execute(command(url), &AtomicBool::new(false), MIN_INTERVAL).unwrap();
                println!("{name}: {probe:?}");
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn startup_is_immediate_and_slow_route_does_not_delay_fast_route() {
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let (release_tx, release_rx) = mpsc::channel();
    let (trigger1, rx1) = mpsc::sync_channel(1);
    let (trigger2, rx2) = mpsc::sync_channel(1);
    let flag = stop.clone();
    let sender = tx.clone();
    let slow = thread::spawn(move || {
        run_with(0, Duration::from_secs(300), sender, rx1, flag, || {
            release_rx.recv().unwrap();
            Some(reply(200))
        })
    });
    let flag = stop.clone();
    let fast = thread::spawn(move || {
        run_with(1, Duration::from_secs(300), tx, rx2, flag, || {
            Some(reply(405))
        })
    });
    let Update::Network(update) = rx.recv_timeout(Duration::from_secs(2)).unwrap() else {
        panic!()
    };
    assert_eq!(update.route, 1);
    // Coalesce in-flight requests; none may overlap the blocked call.
    for _ in 0..100 {
        let _ = trigger1.try_send(());
    }
    release_tx.send(()).unwrap();
    let Update::Network(update) = rx.recv_timeout(Duration::from_secs(2)).unwrap() else {
        panic!()
    };
    assert_eq!(update.route, 0);
    assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());
    stop.store(true, Ordering::Relaxed);
    drop((trigger1, trigger2));
    slow.join().unwrap();
    fast.join().unwrap();
}

#[test]
fn refresh_is_coalesced_and_waits_for_minimum_and_server_backoff() {
    for (age, backoff) in [
        (Duration::from_millis(4850), Duration::ZERO),
        (Duration::from_secs(10), Duration::from_millis(150)),
    ] {
        let (tx, rx) = mpsc::sync_channel(1);
        tx.try_send(()).unwrap();
        assert!(tx.try_send(()).is_err());
        let now = Instant::now();
        assert!(wait_for_next(
            now - age,
            now,
            Duration::from_secs(300),
            backoff,
            &rx,
            &AtomicBool::new(false)
        ));
        assert!(now.elapsed() >= Duration::from_millis(145));
        assert!(now.elapsed() < Duration::from_secs(2));
    }
}
