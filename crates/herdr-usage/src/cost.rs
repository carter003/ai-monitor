//! Pricing: alias resolution (§6.4), the cost formula (§6.1) and the
//! unresolved-model ledger (§6.4).
//!
//! Both tables are re-read once per round: they are tiny and sqlite serves them
//! from its page cache, so an AI price-table update takes effect within 60s
//! without restarting the collector (and therefore without risking the watermarks).

use crate::{db, event::UsageEvent};
use rusqlite::Connection;
use std::collections::HashMap;

/// `model_alias` plus `model_price` folded into one lookup.
#[derive(Debug, Default)]
pub struct PriceTable {
    /// raw model string -> resolution
    aliases: HashMap<String, Alias>,
    /// model_price.model_id -> prices
    prices: HashMap<String, Price>,
    /// last path segment of a price id -> prices, for the prefix-strip fallback.
    /// Ambiguity is resolved by dropping the entry: if two vendors publish the
    /// same bare name, guessing which one a client meant would silently mis-bill.
    by_bare_name: HashMap<String, Price>,
}

#[derive(Debug, Clone)]
pub enum Alias {
    /// Known and deliberately not priced (`ignore = 1`). Excluded from both the
    /// numerator and the denominator of the coverage ratio.
    Ignored,
    /// Mapped to a `model_price` row.
    Mapped(String),
}

#[derive(Debug, Clone, Copy)]
pub struct Price {
    pub prompt: f64,
    pub completion: f64,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}

/// Outcome of pricing one event.
#[derive(Debug, Clone, PartialEq)]
pub enum Priced {
    /// A cost was computed.
    Cost(f64),
    /// Known model, deliberately not priced.
    Ignored,
    /// No alias and no price row: callers record it in `unresolved_model`.
    Unresolved,
}

impl PriceTable {
    pub fn load(connection: &Connection) -> rusqlite::Result<Self> {
        let mut table = PriceTable::default();
        {
            let mut statement =
                connection.prepare("SELECT raw_model, model_id, ignore FROM model_alias")?;
            let rows = statement.query_map([], |row| {
                let raw: String = row.get(0)?;
                let model_id: Option<String> = row.get(1)?;
                let ignore: i64 = row.get(2)?;
                Ok((raw, model_id, ignore))
            })?;
            for row in rows {
                let (raw, model_id, ignore) = row?;
                let alias = match (ignore, model_id) {
                    (1, _) => Alias::Ignored,
                    (_, Some(model_id)) => Alias::Mapped(model_id),
                    (_, None) => Alias::Ignored,
                };
                table.aliases.insert(raw, alias);
            }
        }
        {
            let mut statement = connection.prepare(
                "SELECT model_id, prompt, completion, cache_read, cache_write FROM model_price",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    Price {
                        prompt: row.get(1)?,
                        completion: row.get(2)?,
                        cache_read: row.get(3)?,
                        cache_write: row.get(4)?,
                    },
                ))
            })?;
            for row in rows {
                let (model_id, price) = row?;
                if let Some(bare) = model_id.rsplit('/').next() {
                    if table.by_bare_name.insert(bare.to_owned(), price).is_some() {
                        // Two price ids share this bare name; drop it so the
                        // fallback cannot pick the wrong vendor.
                        table.by_bare_name.remove(bare);
                    }
                }
                table.prices.insert(model_id, price);
            }
        }
        Ok(table)
    }

    /// Resolution order (plan §6.4): explicit alias, then the raw name, then the
    /// name with any vendor prefix stripped.
    ///
    /// The stripped form has to match the price id's own last segment, because
    /// OpenRouter ids are `vendor/model` while two of the four clients report a
    /// bare `model` or their own provider prefix (`opencode-go/deepseek-v4-flash`
    /// must still find `deepseek/deepseek-v4-flash`).
    pub fn resolve(&self, raw_model: &str, usage: &UsageEvent) -> Priced {
        if let Some(alias) = self.aliases.get(raw_model) {
            return match alias {
                Alias::Ignored => Priced::Ignored,
                Alias::Mapped(model_id) => match self.prices.get(model_id) {
                    Some(price) => Priced::Cost(cost(usage, price)),
                    None => Priced::Unresolved,
                },
            };
        }
        if let Some(price) = self.prices.get(raw_model) {
            return Priced::Cost(cost(usage, price));
        }
        let bare = raw_model.rsplit('/').next().unwrap_or(raw_model);
        if bare != raw_model {
            if let Some(price) = self.prices.get(bare) {
                return Priced::Cost(cost(usage, price));
            }
        }
        match self.by_bare_name.get(bare) {
            Some(price) => Priced::Cost(cost(usage, price)),
            None => Priced::Unresolved,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.prices.is_empty() && self.aliases.is_empty()
    }

    /// Number of priced model ids, for the importer's summary output.
    pub fn price_count(&self) -> usize {
        self.prices.len()
    }
}

/// Cost formula (plan §6.1, unique owner).
///
/// * Fresh input is `input_total - cache_read`, so cached tokens are not billed
///   twice at full price.
/// * `cache_write` falls back to the prompt price when the price row has no
///   `input_cache_write` (writing a cache entry is normally dearer than a plain
///   prompt read, so falling back to 0 would understate the bill).
pub fn cost(usage: &UsageEvent, price: &Price) -> f64 {
    let fresh_input = usage.input_total.saturating_sub(usage.cache_read).max(0);
    let cache_read_price = price.cache_read.unwrap_or(price.prompt);
    let cache_write_price = price.cache_write.unwrap_or(price.prompt);
    (fresh_input as f64) * price.prompt
        + (usage.cache_read.max(0) as f64) * cache_read_price
        + (usage.cache_write.max(0) as f64) * cache_write_price
        + (usage.output_total.max(0) as f64) * price.completion
}

/// Record or refresh an entry in the unresolved ledger.
///
/// `hit_count` accumulates across rounds and `last_seen` drives the 7-day
/// reminder window, so an abandoned model ages out of the reminder list instead
/// of permanently occupying its head.
pub fn note_unresolved(
    connection: &Connection,
    source: &str,
    raw_model: &str,
    now_ms: i64,
) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO unresolved_model(source, raw_model, hit_count, first_seen, last_seen)
         VALUES (?1, ?2, 1, ?3, ?3)
         ON CONFLICT(source, raw_model) DO UPDATE SET
             hit_count = hit_count + 1,
             last_seen = excluded.last_seen",
        rusqlite::params![source, raw_model, now_ms],
    )?;
    Ok(())
}

/// One row of the reminder list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unresolved {
    pub raw_model: String,
    pub source: String,
    pub hit_count: i64,
    pub first_seen: i64,
    pub last_seen: i64,
}

/// The reminder list: unresolved models seen inside the window, busiest first.
pub fn unresolved_within(
    connection: &Connection,
    since_ms: i64,
) -> rusqlite::Result<Vec<Unresolved>> {
    let mut statement = connection.prepare(
        "SELECT raw_model, source, hit_count, first_seen, last_seen
         FROM unresolved_model
         WHERE last_seen >= ?1
         ORDER BY hit_count DESC, raw_model ASC",
    )?;
    let rows = statement.query_map([since_ms], |row| {
        Ok(Unresolved {
            raw_model: row.get(0)?,
            source: row.get(1)?,
            hit_count: row.get(2)?,
            first_seen: row.get(3)?,
            last_seen: row.get(4)?,
        })
    })?;
    rows.collect()
}

/// Recalculate cost for historical events where `cost_usd IS NULL` and `model IS NOT NULL`.
///
/// If `price_table.resolve` produces a price (`Priced::Cost`), the event's `cost_usd` is updated.
/// Returns the number of events that were repriced.
pub fn reprice_unpriced_events(
    connection: &mut Connection,
    price_table: &PriceTable,
) -> rusqlite::Result<usize> {
    struct UnpricedRow {
        source: String,
        event_id: String,
        model: String,
        input_total: i64,
        cache_read: i64,
        cache_write: i64,
        output_total: i64,
    }
    let rows: Vec<UnpricedRow> = {
        let mut select_stmt = connection.prepare(
            "SELECT source, event_id, model, input_total, cache_read, cache_write, output_total
             FROM usage_event
             WHERE cost_usd IS NULL AND model IS NOT NULL",
        )?;
        let rows = select_stmt
            .query_map([], |row| {
                Ok(UnpricedRow {
                    source: row.get(0)?,
                    event_id: row.get(1)?,
                    model: row.get(2)?,
                    input_total: row.get(3)?,
                    cache_read: row.get(4)?,
                    cache_write: row.get(5)?,
                    output_total: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };

    let mut updates: Vec<(f64, String, String)> = Vec::new();
    for row in rows {
        let usage = UsageEvent {
            input_total: row.input_total,
            cache_read: row.cache_read,
            cache_write: row.cache_write,
            output_total: row.output_total,
            reasoning: 0,
        };
        if let Priced::Cost(c) = price_table.resolve(&row.model, &usage) {
            updates.push((c, row.source, row.event_id));
        }
    }

    if updates.is_empty() {
        return Ok(0);
    }

    let tx = connection.transaction()?;
    {
        let mut update_stmt = tx.prepare(
            "UPDATE usage_event SET cost_usd = ?1 WHERE source = ?2 AND event_id = ?3",
        )?;
        for (cost, source, event_id) in &updates {
            update_stmt.execute(rusqlite::params![cost, source, event_id])?;
        }
    }
    tx.commit()?;

    Ok(updates.len())
}

pub const WINDOW_7D_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Load both tables for one round.
pub fn load(connection: &Connection) -> rusqlite::Result<PriceTable> {
    let _ = db::SCHEMA;
    PriceTable::load(connection)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input_total: i64, cache_read: i64, cache_write: i64, output: i64) -> UsageEvent {
        UsageEvent {
            input_total,
            cache_read,
            cache_write,
            output_total: output,
            reasoning: 0,
        }
    }

    // Real OpenRouter values recorded in plan §8.3.
    const GPT_5_3_CODEX: Price = Price {
        prompt: 0.00000175,
        completion: 0.000014,
        cache_read: Some(0.000000175),
        cache_write: None,
    };

    #[test]
    fn cached_tokens_are_not_billed_twice_at_full_price() {
        // 1_000_000 gross input of which 900_000 were cache hits.
        let value = cost(&usage(1_000_000, 900_000, 0, 100_000), &GPT_5_3_CODEX);
        let expected = 100_000.0 * 0.00000175 + 900_000.0 * 0.000000175 + 100_000.0 * 0.000014;
        assert!((value - expected).abs() < 1e-12, "{value} != {expected}");
    }

    #[test]
    fn cache_write_falls_back_to_the_prompt_price_not_zero() {
        let price = Price {
            cache_read: Some(0.0000001),
            cache_write: None,
            ..GPT_5_3_CODEX
        };
        let value = cost(&usage(0, 0, 1_000_000, 0), &price);
        assert!((value - 1_000_000.0 * 0.00000175).abs() < 1e-12);
        assert!(value > 0.0, "silently billing a cache write as free");
    }

    #[test]
    fn an_explicit_cache_write_price_wins_over_the_fallback() {
        let price = Price {
            cache_write: Some(0.000005),
            ..GPT_5_3_CODEX
        };
        let value = cost(&usage(0, 0, 1_000_000, 0), &price);
        assert!((value - 5.0).abs() < 1e-12, "{value}");
    }

    #[test]
    fn reasoning_is_never_billed_separately() {
        let with_reasoning = UsageEvent {
            reasoning: 500_000,
            ..usage(0, 0, 0, 1_000_000)
        };
        let without = usage(0, 0, 0, 1_000_000);
        assert_eq!(
            cost(&with_reasoning, &GPT_5_3_CODEX),
            cost(&without, &GPT_5_3_CODEX)
        );
    }

    #[test]
    fn fresh_input_never_goes_negative_when_cache_exceeds_it() {
        let value = cost(&usage(10, 100, 0, 0), &GPT_5_3_CODEX);
        assert!(value >= 0.0);
        assert!((value - 100.0 * 0.000000175).abs() < 1e-12);
    }

    fn seeded() -> Connection {
        let connection = Connection::open_in_memory().expect("memory db");
        db::initialize(&connection).expect("schema");
        connection
            .execute(
                "INSERT INTO model_price(model_id, prompt, completion, cache_read, cache_write, remark, updated_at)
                 VALUES ('openai/gpt-5.3-codex', 0.00000175, 0.000014, 0.000000175, NULL, NULL, 0)",
                [],
            )
            .expect("price");
        connection
            .execute(
                "INSERT INTO model_price(model_id, prompt, completion, cache_read, cache_write, remark, updated_at)
                 VALUES ('deepseek/deepseek-v4-flash', 0.00000008708, 0.00000017416, 0.000000017416, NULL, NULL, 0)",
                [],
            )
            .expect("price");
        connection
            .execute(
                "INSERT INTO model_alias(raw_model, model_id, ignore, resolved_by, remark)
                 VALUES ('gpt-5.3-codex-spark', 'openai/gpt-5.3-codex', 0, 'manual', 'spark 变体无独立定价，按 gpt-5.3-codex 计价（用户确认）')",
                [],
            )
            .expect("alias");
        connection
            .execute(
                "INSERT INTO model_alias(raw_model, model_id, ignore, resolved_by, remark)
                 VALUES ('opencode/deepseek-v4-flash-free', NULL, 1, 'ignore', '免费通道不计价')",
                [],
            )
            .expect("ignore alias");
        connection
    }

    #[test]
    fn an_explicit_alias_wins_over_the_raw_name() {
        let table = PriceTable::load(&seeded()).expect("load");
        // `gpt-5.3-codex-spark` has no OpenRouter row of its own; the alias must
        // route it onto the gpt-5.3-codex prices (plan §6.3).
        let priced = table.resolve("gpt-5.3-codex-spark", &usage(0, 0, 0, 1_000_000));
        match priced {
            Priced::Cost(value) => {
                assert!(
                    (value - 14.0).abs() < 1e-9,
                    "expected 1M output at 0.000014, got {value}"
                )
            }
            other => panic!("alias did not resolve: {other:?}"),
        }
    }

    #[test]
    fn ignore_short_circuits_and_is_distinguishable_from_unpriced() {
        let table = PriceTable::load(&seeded()).expect("load");
        assert_eq!(
            table.resolve("opencode/deepseek-v4-flash-free", &usage(0, 0, 0, 0)),
            Priced::Ignored
        );
        assert_eq!(
            table.resolve("big-pickle", &usage(0, 0, 0, 0)),
            Priced::Unresolved
        );
    }

    #[test]
    fn a_vendor_prefix_is_stripped_before_giving_up() {
        let table = PriceTable::load(&seeded()).expect("load");
        // omp reports the same model under a route prefix.
        let priced = table.resolve("opencode-go/deepseek-v4-flash", &usage(0, 0, 0, 1_000_000));
        match priced {
            Priced::Cost(value) => assert!(value > 0.0, "prefix-stripped lookup returned 0"),
            other => panic!("vendor prefix was not stripped: {other:?}"),
        }
    }

    #[test]
    fn an_unpriced_model_is_reported_rather_than_guessed() {
        let table = PriceTable::load(&seeded()).expect("load");
        assert_eq!(
            table.resolve("brand-new-model", &usage(0, 0, 0, 0)),
            Priced::Unresolved
        );
    }

    #[test]
    fn the_reminder_window_excludes_models_last_seen_before_it() {
        let connection = seeded();
        let now = 1_800_000_000_000i64;
        // A high-traffic abandoned model, last seen 30 days ago.
        connection
            .execute(
                "INSERT INTO unresolved_model(source, raw_model, hit_count, first_seen, last_seen)
                 VALUES ('opencode', 'retired-model', 500, ?1, ?2)",
                rusqlite::params![now - 40 * 24 * 3600 * 1000, now - 30 * 24 * 3600 * 1000],
            )
            .expect("old row");
        note_unresolved(&connection, "opencode", "big-pickle", now - 60_000).expect("note");
        let recent = unresolved_within(&connection, now - WINDOW_7D_MS).expect("query");
        let names: Vec<&str> = recent.iter().map(|row| row.raw_model.as_str()).collect();
        assert_eq!(
            names,
            vec!["big-pickle"],
            "the 30-day-old model must age out"
        );
    }

    #[test]
    fn repeated_hits_accumulate_without_duplicating_the_row() {
        let connection = seeded();
        let now = 1_800_000_000_000i64;
        for offset in 0..5 {
            note_unresolved(&connection, "omp", "big-pickle", now + offset).expect("note");
        }
        let rows = unresolved_within(&connection, now - WINDOW_7D_MS).expect("query");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hit_count, 5);
        assert_eq!(rows[0].last_seen, now + 4);
    }
    #[test]
    fn reprice_unpriced_events_updates_null_cost_rows() {
        let mut connection = seeded();
        // Insert an unpriced event with a model that can resolve (e.g. gpt-5.3-codex-spark)
        connection
            .execute(
                "INSERT INTO usage_event(source, event_id, model, model_source, input_total, cache_read, cache_write, output_total, reasoning, cost_usd, occurred_at)
                 VALUES ('codex', 'evt-1', 'gpt-5.3-codex-spark', 'context', 1000, 0, 0, 1000, 0, NULL, 100)",
                [],
            )
            .expect("insert evt-1");
        // Insert an unpriced event with an ignored model
        connection
            .execute(
                "INSERT INTO usage_event(source, event_id, model, model_source, input_total, cache_read, cache_write, output_total, reasoning, cost_usd, occurred_at)
                 VALUES ('opencode', 'evt-2', 'opencode/deepseek-v4-flash-free', 'event', 1000, 0, 0, 1000, 0, NULL, 100)",
                [],
            )
            .expect("insert evt-2");
        // Insert an unpriced event with an unresolved model
        connection
            .execute(
                "INSERT INTO usage_event(source, event_id, model, model_source, input_total, cache_read, cache_write, output_total, reasoning, cost_usd, occurred_at)
                 VALUES ('omp', 'evt-3', 'unknown-model-xyz', 'event', 1000, 0, 0, 1000, 0, NULL, 100)",
                [],
            )
            .expect("insert evt-3");

        let table = PriceTable::load(&connection).expect("load");
        let repriced = reprice_unpriced_events(&mut connection, &table).expect("reprice");
        assert_eq!(repriced, 1);

        let cost_1: Option<f64> = connection
            .query_row("SELECT cost_usd FROM usage_event WHERE event_id = 'evt-1'", [], |r| r.get(0))
            .expect("query evt-1");
        assert!(cost_1.is_some());
        assert!(cost_1.unwrap() > 0.0);

        let cost_2: Option<f64> = connection
            .query_row("SELECT cost_usd FROM usage_event WHERE event_id = 'evt-2'", [], |r| r.get(0))
            .expect("query evt-2");
        assert_eq!(cost_2, None);

        let cost_3: Option<f64> = connection
            .query_row("SELECT cost_usd FROM usage_event WHERE event_id = 'evt-3'", [], |r| r.get(0))
            .expect("query evt-3");
        assert_eq!(cost_3, None);
    }
}
