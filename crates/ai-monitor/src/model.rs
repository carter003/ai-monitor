use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Codex,
    Agy,
    Agy2,
    Go,
    Go2,
    Grok,
    OpenRouter,
}

impl Source {
    pub const ALL: [Self; 7] = [
        Self::Codex,
        Self::Agy,
        Self::Agy2,
        Self::Go,
        Self::Go2,
        Self::Grok,
        Self::OpenRouter,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Codex => "Codex · GPT",
            Self::Agy => "AGY",
            Self::Agy2 => "AGY2",
            Self::Go => "OpenCode Go",
            Self::Go2 => "OpenCode GO-2",
            Self::Grok => "SuperGrok",
            Self::OpenRouter => "OpenRouter",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Meter {
    pub label: String,
    pub remaining: Option<f64>,
    pub resets_at: Option<i64>,
    pub available: bool,
    /// 官方数值自带的小数位数（上限 2），显示时至少保留该精度。
    pub decimals: u8,
}

impl Meter {
    pub fn from_used(
        label: impl Into<String>,
        used: f64,
        resets_at: Option<i64>,
        decimals: u8,
    ) -> Result<Self, FetchError> {
        if !used.is_finite() || used < 0.0 {
            return Err(FetchError::format());
        }
        Ok(Self {
            label: label.into(),
            remaining: Some((100.0 - used).clamp(0.0, 100.0)),
            resets_at,
            available: false,
            decimals: decimals.min(2),
        })
    }

    pub fn expired(&self, now: i64) -> bool {
        self.resets_at.is_some_and(|t| t <= now)
    }
}

#[derive(Clone, Debug)]
pub struct Card {
    pub title: String,
    pub meters: Vec<Meter>,
    pub balance: Option<f64>,
    pub note: Option<String>,
}

impl Card {
    pub fn empty(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            meters: vec![],
            balance: None,
            note: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Local token consumption
//
// These types describe tokens actually burned on this machine, which is a
// different thing from the cloud quota remaining above: its `Source`/`Meter`/
// `Card` are "how much is left", these are "how much was used". The two sets of
// numbers appear on the same screen, so the panel titles are the only defence
// against confusing them.
// ---------------------------------------------------------------------------

/// Compact representation of numbers, e.g. `1.5B` / `12.4M` / `900K`.
pub fn compact(value: u64) -> String {
    match value {
        1_000_000_000.. => format!("{:.1}B", value as f64 / 1e9),
        1_000_000.. => format!("{:.1}M", value as f64 / 1e6),
        1_000.. => format!("{}K", value / 1_000),
        _ => value.to_string(),
    }
}

/// One model's usage totals.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelUsage {
    pub model: String,
    pub input_total: u64,
    pub cache_read: u64,
    pub output: u64,
    pub reasoning: u64,
    /// `None` when every event for this model was unpriced.
    pub cost: Option<f64>,
}

impl ModelUsage {
    /// Ranking quantity: gross input plus gross output. Cache reads are real
    /// context that really crossed the wire, so they count.
    pub fn total_tokens(&self) -> u64 {
        self.input_total.saturating_add(self.output)
    }

    /// Cache hit share as a percentage with exactly two decimals, truncated
    /// rather than rounded so a ratio one hundredth below a boundary never
    /// reads as the boundary itself. Integer arithmetic keeps the truncation
    /// exact: `97.00%` and `87.50%`, never `97.01%` or `87.51%`.
    pub fn hit_display(&self) -> Option<String> {
        (self.input_total > 0).then(|| {
            let basis_points = self.cache_read as u128 * 10_000 / self.input_total as u128;
            format!("{}.{:02}%", basis_points / 100, basis_points % 100)
        })
    }

    /// Formatted input total and hit ratio for display, e.g. "390.8M(97.00%)" or "0(—)".
    pub fn input_display(&self) -> String {
        match self.hit_display() {
            Some(hit) => format!("{}({hit})", compact(self.input_total)),
            None => "0(—)".into(),
        }
    }
}

/// Token totals for one interval.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UsageTotal {
    /// Gross input plus gross output.
    pub tokens: u64,
    /// Sum of priceable events only; unpriced events contribute nothing.
    pub cost: f64,
}

/// A histogram whose buckets come from calendar fields, so the caller supplies
/// the bucket count and the labels rather than a fixed width.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bucketed {
    /// One entry per bucket; empty buckets are present as zeros so the bars line
    /// up with their labels.
    pub buckets: Vec<u64>,
    /// Money spent in each bucket, index-aligned with `buckets`. Carried beside
    /// the tokens so the money chart reuses the same query and buckets.
    pub costs: Vec<f64>,
}

/// Everything the token page needs, refreshed on its own slow cadence.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageStats {
    /// Rolling 24-hour top six, already ordered by total tokens descending.
    pub models: Vec<ModelUsage>,
    /// Current-month top ten, already ordered by total tokens descending.
    pub month_models: Vec<ModelUsage>,
    /// Today, cut into quarter hours: 96 buckets, index 0 is 00:00 local.
    pub hours: Bucketed,
    /// This month, cut into six-hour blocks: four buckets per day, index 0 is
    /// the first day at 00:00 local.
    pub month: Bucketed,
    pub day_total: UsageTotal,
    pub week_total: UsageTotal,
    pub month_total: UsageTotal,
    pub year_total: UsageTotal,
    /// Whole-table totals; the homepage summary row shows the same figures.
    pub all_total: UsageTotal,
    /// Set when the database could not be read, so the homepage can say so
    /// instead of rendering zeros that look like real data.
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct FetchError {
    pub message: String,
    pub retry_after: Option<Duration>,
    pub invalidate: bool,
}

impl FetchError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retry_after: None,
            invalidate: false,
        }
    }

    pub fn auth(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retry_after: None,
            invalidate: true,
        }
    }

    pub fn format() -> Self {
        Self::new("额度数据不完整")
    }
}

pub struct SourceState {
    pub source: Source,
    pub identity: Option<String>,
    pub cards: Vec<Card>,
    pub fetched_at: Option<i64>,
    pub error: Option<String>,
    pub refreshing: bool,
    pub next_fetch: Instant,
    pub refresh_interval: Duration,
    /// 周额度用尽后的等重置时间（wall clock）。此后 worker 停轮询，UI 显示等待而非旧数据。
    pub hold_until: Option<i64>,
}

impl SourceState {
    pub fn new(source: Source, refresh_interval: Duration) -> Self {
        Self {
            source,
            identity: None,
            cards: Self::placeholders(source),
            fetched_at: None,
            error: None,
            refreshing: true,
            next_fetch: Instant::now(),
            refresh_interval,
            hold_until: None,
        }
    }

    fn placeholders(source: Source) -> Vec<Card> {
        let mut cards = vec![Card::empty(source.title())];
        if source == Source::Codex {
            cards.push(Card::empty("Codex · 5.3 Spark"));
        }
        cards
    }

    pub fn begin(&mut self, identity: String) {
        if self.identity.as_deref() != Some(&identity) {
            self.cards = Self::placeholders(self.source);
            self.fetched_at = None;
            self.error = None;
            self.hold_until = None;
        }
        self.identity = Some(identity);
        self.refreshing = true;
    }

    pub fn finish(&mut self, result: Result<Vec<Card>, FetchError>, now: i64, next: Instant) {
        self.refreshing = false;
        self.next_fetch = next;
        match result {
            Ok(cards) => {
                self.hold_until = weekly_hold_until(&cards, now);
                self.cards = cards;
                self.fetched_at = Some(now);
                self.error = None;
            }
            Err(e) => {
                // A failed manual refresh resumes retries; it is no longer a hold.
                self.hold_until = None;
                if e.invalidate {
                    self.cards = Self::placeholders(self.source);
                    self.fetched_at = None;
                    self.identity = None;
                }
                self.error = Some(e.message);
            }
        }
    }
}

/// Only hold a source when every returned independent pool is exhausted.
/// Empty cards describe unavailable pools (such as an absent Spark subscription).
pub fn weekly_hold_until(cards: &[Card], now: i64) -> Option<i64> {
    let mut until = None;
    for card in cards
        .iter()
        .filter(|card| !card.meters.is_empty() || card.balance.is_some())
    {
        let reset = card
            .meters
            .iter()
            .filter(|meter| meter.label == "周" && meter.remaining.is_some_and(|v| v <= 0.0))
            .filter_map(|meter| meter.resets_at)
            .filter(|at| *at > now)
            .min()?;
        until = Some(until.map_or(reset, |at: i64| at.min(reset)));
    }
    until
}

pub fn window_label(seconds: i64) -> String {
    match seconds {
        18000 => "5H".into(),
        604800 => "周".into(),
        s if s > 0 && s % 86400 == 0 => format!("{}天", s / 86400),
        s if s > 0 && s % 3600 == 0 => format!("{}H", s / 3600),
        s if s > 0 => format!("{}m", s / 60),
        _ => "额度".into(),
    }
}

pub fn countdown(seconds: i64) -> String {
    if seconds <= 0 {
        return "待刷新".into();
    }
    let days = seconds / 86400;
    let hours = seconds % 86400 / 3600;
    let minutes = seconds % 3600 / 60;
    if days > 0 {
        format!("{days}d {hours:02}h")
    } else if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m {:02}s", seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_switch_cannot_reuse_quota() {
        let mut state = SourceState::new(Source::Agy, std::time::Duration::from_secs(60));
        state.begin("account-a".into());
        state.finish(
            Ok(vec![Card {
                meters: vec![Meter::from_used("周", 98., None, 0).unwrap()],
                ..Card::empty("A")
            }]),
            100,
            Instant::now(),
        );
        state.begin("account-b".into());
        assert!(state.cards[0].meters.is_empty());
        assert!(state.fetched_at.is_none());
    }

    #[test]
    fn failure_preserves_time_and_does_not_reset_quota() {
        let mut state = SourceState::new(Source::Go, std::time::Duration::from_secs(60));
        state.begin("a".into());
        state.finish(
            Ok(vec![Card {
                meters: vec![Meter::from_used("月", 98., Some(110), 0).unwrap()],
                ..Card::empty("Go")
            }]),
            100,
            Instant::now(),
        );
        state.finish(Err(FetchError::new("超时")), 120, Instant::now());
        assert_eq!(state.fetched_at, Some(100));
        assert_eq!(state.cards[0].meters[0].remaining, Some(2.));
        assert!(state.cards[0].meters[0].expired(120));
    }

    #[test]
    fn authorization_loss_clears_prior_readings() {
        let mut state = SourceState::new(Source::Grok, std::time::Duration::from_secs(60));
        state.begin("a".into());
        state.finish(Ok(vec![Card::empty("Grok")]), 100, Instant::now());
        state.finish(Err(FetchError::auth("登录已过期")), 120, Instant::now());
        assert!(state.fetched_at.is_none());
        assert!(state.identity.is_none());
    }
    #[test]
    fn windows_are_identified_by_duration() {
        assert_eq!(window_label(604800), "周");
        assert_eq!(window_label(18000), "5H");
        assert!(Meter::from_used("周", f64::NAN, None, 0).is_err());
    }

    #[test]
    fn exhausted_weekly_quota_records_hold_until_reset() {
        let mut state = SourceState::new(Source::Agy, std::time::Duration::from_secs(60));
        state.begin("a".into());
        let exhausted = || {
            Ok(vec![Card {
                meters: vec![Meter::from_used("周", 100., Some(200), 1).unwrap()],
                ..Card::empty("A")
            }])
        };
        state.finish(exhausted(), 100, Instant::now());
        assert_eq!(state.hold_until, Some(200));
        assert_eq!(weekly_hold_until(&state.cards, 100), Some(200));
        // 额度恢复后不再等待；授权失效清占位时也不留等待。
        state.finish(
            Ok(vec![Card {
                meters: vec![Meter::from_used("周", 20., Some(200), 1).unwrap()],
                ..Card::empty("A")
            }]),
            150,
            Instant::now(),
        );
        assert_eq!(state.hold_until, None);
        state.finish(exhausted(), 150, Instant::now());
        state.finish(Err(FetchError::auth("登录已过期")), 160, Instant::now());
        assert_eq!(state.hold_until, None);
    }
    #[test]
    fn model_usage_input_display_formats_correctly() {
        let m1 = ModelUsage {
            model: "test".into(),
            input_total: 390_800_000,
            cache_read: 379_076_000,
            output: 10_000,
            reasoning: 0,
            cost: None,
        };
        assert_eq!(m1.input_display(), "390.8M(97.00%)");

        let m2 = ModelUsage {
            model: "test2".into(),
            input_total: 869_000,
            cache_read: 799_480,
            output: 1_000,
            reasoning: 0,
            cost: None,
        };
        assert_eq!(m2.input_display(), "869K(92.00%)");

        let m_zero = ModelUsage::default();
        assert_eq!(m_zero.input_display(), "0(—)");
    }

    #[test]
    fn cache_hit_share_is_truncated_at_two_decimals_not_rounded() {
        // 1/3 = 33.333…%: the third decimal is dropped, not rounded up.
        let third = ModelUsage {
            input_total: 3,
            cache_read: 1,
            ..ModelUsage::default()
        };
        assert_eq!(third.hit_display().as_deref(), Some("33.33%"));

        // 96.666…% would round to 96.67%, but truncation keeps 96.66%.
        let two_thirds = ModelUsage {
            input_total: 30,
            cache_read: 29,
            ..ModelUsage::default()
        };
        assert_eq!(two_thirds.hit_display().as_deref(), Some("96.66%"));

        // An exact ratio keeps both decimals rather than dropping to an integer.
        let exact = ModelUsage {
            input_total: 8,
            cache_read: 7,
            ..ModelUsage::default()
        };
        assert_eq!(exact.hit_display().as_deref(), Some("87.50%"));

        // Nothing sent: no ratio to print.
        assert_eq!(ModelUsage::default().hit_display(), None);
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    #[test]
    fn account_switch_clears_previous_hold() {
        let mut state = SourceState::new(Source::Agy, std::time::Duration::from_secs(60));
        state.begin("account-a".into());
        state.finish(
            Ok(vec![Card {
                meters: vec![Meter::from_used("周", 100., Some(10000), 0).unwrap()],
                ..Card::empty("A")
            }]),
            100,
            Instant::now(),
        );
        state.begin("account-b".into());
        assert_eq!(state.hold_until, None);
        state.finish(Err(FetchError::new("连接超时")), 200, Instant::now());
        assert_eq!(state.hold_until, None);
    }
}