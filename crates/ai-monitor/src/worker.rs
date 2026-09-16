use crate::{
    config::Config,
    http::Http,
    model::{Card, FetchError, Source, UsageStats, weekly_hold_until},
    providers,
    usage::Reader,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

pub enum Update {
    Started(Source, String),
    Finished(Source, Result<Vec<Card>, FetchError>, i64, Instant),
    /// Local token consumption, refreshed on its own cadence or on demand when
    /// the user presses `r`.
    Usage(Box<UsageStats>),
}

/// How often the token page re-reads the collector database. Independent of
/// `config.refresh`: the collector itself only writes every 60 seconds, so
/// polling faster would burn a read for no new data.
const USAGE_INTERVAL: Duration = Duration::from_secs(60);

/// Back-off after the database could not be opened or read.
const USAGE_RETRY: Duration = Duration::from_secs(300);

pub struct Workers {
    pub updates: Receiver<Update>,
    senders: Vec<SyncSender<()>>,
    stop: Arc<AtomicBool>,
}

impl Workers {
    pub fn start(config: Config) -> Self {
        let (tx, updates) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let mut senders = vec![];
        // Client clones share their pools and blocking runtimes. Credentials and
        // OAuth sessions remain per worker; authentication is request-specific.
        let http = Http::new();
        for source in Source::ALL {
            let (trigger, receiver) = mpsc::sync_channel(1);
            senders.push(trigger);
            let config = config.clone();
            let tx = tx.clone();
            let stop = stop.clone();
            let http = http.clone();
            thread::spawn(move || {
                let http = match http {
                    Ok(http) => http,
                    Err(e) => {
                        let _ = tx.send(Update::Finished(
                            source,
                            Err(e),
                            chrono::Utc::now().timestamp(),
                            Instant::now() + Duration::from_secs(60),
                        ));
                        return;
                    }
                };
                let mut failures = 0u32;
                let mut session = providers::Session::default();
                loop {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let result = match providers::prepare(source, &config) {
                        Ok(input) => {
                            if tx
                                .send(Update::Started(source, input.identity.clone()))
                                .is_err()
                            {
                                break;
                            }
                            providers::fetch(source, &input, &http, &config, &mut session)
                        }
                        Err(error) => Err(error),
                    };
                    let now = Instant::now();
                    let not_before;
                    let wait = if let Err(error) = &result {
                        failures = failures.saturating_add(1);
                        let delay = error
                            .retry_after
                            .unwrap_or_else(|| error_delay(config.refresh, failures));
                        // A server-requested delay cannot be bypassed by pressing r.
                        not_before = now + error.retry_after.unwrap_or(Duration::from_secs(5));
                        delay.max(Duration::from_secs(5))
                    } else {
                        failures = 0;
                        not_before = now + Duration::from_secs(5);
                        next_success_delay(config.refresh, &result, chrono::Utc::now().timestamp())
                    };
                    if tx
                        .send(Update::Finished(
                            source,
                            result,
                            chrono::Utc::now().timestamp(),
                            now + wait,
                        ))
                        .is_err()
                    {
                        break;
                    }
                    // Discard refresh requests accumulated during the in-flight query.
                    while receiver.try_recv().is_ok() {}
                    let until = now + wait;
                    loop {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        let remaining = until.saturating_duration_since(Instant::now());
                        if remaining.is_zero() {
                            break;
                        }
                        match receiver.recv_timeout(remaining.min(Duration::from_secs(1))) {
                            Ok(()) if Instant::now() >= not_before => break,
                            Err(mpsc::RecvTimeoutError::Disconnected) => return,
                            _ => (),
                        }
                    }
                }
            });
        }
        // The usage reader runs on its own thread so a slow or missing database
        // cannot delay the quota polls, and vice versa. It reuses the existing
        // channel so `main` keeps a single `try_iter()` loop.
        let (usage_tx, usage_rx) = mpsc::sync_channel(1);
        senders.push(usage_tx);
        {
            let config = config.clone();
            let tx = tx.clone();
            let stop = stop.clone();
            thread::spawn(move || {
                let mut reader: Option<Reader> = None;
                loop {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let stats = match reader.as_ref() {
                        Some(active) => active.load(config.usage_start),
                        None => Reader::open(&config.usage_db).and_then(|opened| {
                            let loaded = opened.load(config.usage_start);
                            reader = Some(opened);
                            loaded
                        }),
                    };
                    // A failed open, or a file that has since vanished, drops the
                    // handle so the next attempt re-opens it. Retrying every
                    // minute against a missing file would spam the log for no
                    // benefit, so the failure path waits longer.
                    let wait = match stats {
                        Ok(stats) => {
                            if tx.send(Update::Usage(Box::new(stats))).is_err() {
                                break;
                            }
                            USAGE_INTERVAL
                        }
                        Err(error) => {
                            reader = None;
                            let stats = UsageStats {
                                error: Some(error.to_string()),
                                ..UsageStats::default()
                            };
                            if tx.send(Update::Usage(Box::new(stats))).is_err() {
                                break;
                            }
                            USAGE_RETRY
                        }
                    };
                    // Discard refresh requests accumulated during the in-flight query.
                    while usage_rx.try_recv().is_ok() {}
                    let until = Instant::now() + wait;
                    loop {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        let remaining = until.saturating_duration_since(Instant::now());
                        if remaining.is_zero() {
                            break;
                        }
                        match usage_rx.recv_timeout(remaining.min(Duration::from_secs(1))) {
                            Ok(()) => break,
                            Err(mpsc::RecvTimeoutError::Disconnected) => return,
                            _ => (),
                        }
                    }
                }
            });
        }
        Self {
            updates,
            senders,
            stop,
        }
    }

    pub fn refresh(&self) {
        for sender in &self.senders {
            let _ = sender.try_send(());
        }
    }
}

impl Drop for Workers {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn error_delay(base: Duration, failures: u32) -> Duration {
    base.saturating_mul(1u32 << failures.saturating_sub(1).min(4))
        .min(Duration::from_secs(900))
}

fn next_success_delay(
    base: Duration,
    result: &Result<Vec<Card>, FetchError>,
    now: i64,
) -> Duration {
    let Ok(cards) = result.as_ref() else {
        return base;
    };
    // 所有独立额度池的周额度均已用尽，才暂停整个来源。
    // 启动后第一次本来就会问一次（循环先抓取再算等待），不断线重连也能刷新。
    if let Some(at) = weekly_hold_until(cards, now) {
        return Duration::from_secs((at - now + 1) as u64).max(Duration::from_secs(5));
    }
    let reset = cards
        .iter()
        .flat_map(|card| &card.meters)
        .filter_map(|meter| meter.resets_at)
        .filter(|at| *at > now)
        .min();
    reset
        .map(|at| {
            Duration::from_secs((at - now + 1) as u64)
                .max(Duration::from_secs(5))
                .min(base)
        })
        .unwrap_or(base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Meter;
    #[test]
    fn retries_back_off_without_busy_polling() {
        assert_eq!(
            error_delay(Duration::from_secs(60), 1),
            Duration::from_secs(60)
        );
        assert_eq!(
            error_delay(Duration::from_secs(60), 3),
            Duration::from_secs(240)
        );
        assert_eq!(
            error_delay(Duration::from_secs(60), 100),
            Duration::from_secs(900)
        );
    }

    fn ok_card(meters: Vec<Meter>) -> Result<Vec<Card>, FetchError> {
        Ok(vec![Card {
            meters,
            ..Card::empty("T")
        }])
    }

    #[test]
    fn exhausted_weekly_quota_waits_for_reset_instead_of_polling() {
        let base = Duration::from_secs(60);
        let now = 1_000_000;
        let result = ok_card(vec![
            Meter::from_used("5H", 50., Some(now + 60), 1).unwrap(),
            Meter::from_used("周", 100., Some(now + 6 * 86400), 1).unwrap(),
        ]);
        // 5H 一分钟后就重置也不提前问：整组等周额度到期。
        assert_eq!(
            next_success_delay(base, &result, now),
            Duration::from_secs(6 * 86400 + 1)
        );
    }

    #[test]
    fn weekly_wait_needs_a_known_reset_time() {
        let base = Duration::from_secs(60);
        let now = 1_000_000;
        let exhausted = ok_card(vec![Meter::from_used("周", 100., None, 1).unwrap()]);
        assert_eq!(next_success_delay(base, &exhausted, now), base);
        let fresh = ok_card(vec![
            Meter::from_used("周", 20., Some(now + 6 * 86400), 1).unwrap(),
        ]);
        assert_eq!(next_success_delay(base, &fresh, now), base);
        let monthly = ok_card(vec![
            Meter::from_used("月", 100., Some(now + 6 * 86400), 1).unwrap(),
        ]);
        assert_eq!(next_success_delay(base, &monthly, now), base);
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    use crate::model::Meter;
    #[test]
    fn active_independent_pool_keeps_polling() {
        let now = 1_000_000;
        let result = Ok(vec![
            Card {
                meters: vec![Meter::from_used("周", 100., Some(now + 6 * 86400), 0).unwrap()],
                ..Card::empty("GPT")
            },
            Card {
                meters: vec![Meter::from_used("周", 20., Some(now + 86400), 0).unwrap()],
                ..Card::empty("second pool")
            },
        ]);
        assert_eq!(
            next_success_delay(Duration::from_secs(60), &result, now),
            Duration::from_secs(60)
        );
    }
    #[test]
    fn all_independent_pools_resume_at_earliest_weekly_reset() {
        let now = 1_000_000;
        let card = |reset| Card {
            meters: vec![Meter::from_used("周", 100., Some(reset), 0).unwrap()],
            ..Card::empty("pool")
        };
        let cards = vec![card(now + 600), card(now + 300)];
        assert_eq!(
            next_success_delay(Duration::from_secs(60), &Ok(cards.clone()), now),
            Duration::from_secs(301)
        );
        assert_eq!(
            next_success_delay(Duration::from_secs(60), &Ok(cards), now + 301),
            Duration::from_secs(60)
        );
        // An absent subscription placeholder does not prevent an otherwise valid hold.
        let placeholders = vec![card(now + 600), Card::empty("pool unavailable")];
        assert_eq!(
            next_success_delay(Duration::from_secs(60), &Ok(placeholders), now),
            Duration::from_secs(601)
        );
        let unknown = vec![
            card(now + 600),
            Card {
                meters: vec![crate::model::Meter {
                    label: "周".into(),
                    remaining: None,
                    resets_at: None,
                    available: false,
                    decimals: 0,
                }],
                ..Card::empty("unknown")
            },
        ];
        assert_eq!(
            next_success_delay(Duration::from_secs(60), &Ok(unknown), now),
            Duration::from_secs(60)
        );
    }
}
