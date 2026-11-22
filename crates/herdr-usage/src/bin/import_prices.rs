//! Price-table importer — the only writer of `model_price` (plan §8.3).
//!
//! The collector never touches the network; this binary is run manually or by an
//! AI when prices need refreshing.

use herdr_usage::{cost, db};
use serde_json::Value;

const ENDPOINT: &str = "https://openrouter.ai/api/v1/models";

fn main() {
    let database = herdr_usage::db_path();
    let mut connection = match db::open(&database) {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("无法打开数据库 {}：{error}", database.display());
            std::process::exit(1);
        }
    };

    let body = match fetch() {
        Ok(body) => body,
        Err(error) => {
            // A failed fetch leaves the existing table untouched: a stale price
            // beats no price at all.
            eprintln!("拉取价格表失败：{error}");
            eprintln!("现有价格表未改动。");
            std::process::exit(1);
        }
    };

    let parsed = match serde_json::from_str::<Value>(&body) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("价格表 JSON 解析失败：{error}");
            std::process::exit(1);
        }
    };
    let Some(entries) = parsed.get("data").and_then(Value::as_array) else {
        eprintln!("价格表结构异常：缺少 data 数组");
        std::process::exit(1);
    };

    let now = herdr_usage::sources::now_ms();
    let mut written = 0usize;
    let mut skipped: Vec<String> = vec![];
    let transaction = match connection.transaction() {
        Ok(transaction) => transaction,
        Err(error) => {
            eprintln!("无法开启事务：{error}");
            std::process::exit(1);
        }
    };
    // OpenRouter reports `-1` for models with dynamic/unknown pricing. A row
    // with that sentinel bills a negative amount, and a catalogue refresh no
    // longer writes it, so stale rows and their events must be cleared first.
    let (purged_prices, cleared_costs) = match purge_invalid_prices(&transaction) {
        Ok(counts) => counts,
        Err(error) => {
            eprintln!("清理无效价格失败：{error}");
            std::process::exit(1);
        }
    };
    {
        // `remark` is deliberately NOT in the DO UPDATE list: a hand-written
        // deviation note must survive every refresh (plan §8.3).
        let mut statement = match transaction.prepare(
            "INSERT INTO model_price(model_id, prompt, completion, cache_read, cache_write, remark, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)
             ON CONFLICT(model_id) DO UPDATE SET
                 prompt = excluded.prompt,
                 completion = excluded.completion,
                 cache_read = excluded.cache_read,
                 cache_write = excluded.cache_write,
                 updated_at = excluded.updated_at",
        ) {
            Ok(statement) => statement,
            Err(error) => {
                eprintln!("无法准备写入语句：{error}");
                std::process::exit(1);
            }
        };
        for entry in entries {
            let Some(model_id) = entry.get("id").and_then(Value::as_str) else {
                continue;
            };
            let pricing = entry.get("pricing");
            let (Some(prompt), Some(completion)) = (
                pricing.and_then(|value| price(value, "prompt")),
                pricing.and_then(|value| price(value, "completion")),
            ) else {
                // One bad row must not abort the whole import.
                skipped.push(model_id.to_owned());
                continue;
            };
            let cache_read = pricing.and_then(|value| price(value, "input_cache_read"));
            let cache_write = pricing.and_then(|value| price(value, "input_cache_write"));
            if statement
                .execute(rusqlite::params![
                    model_id,
                    prompt,
                    completion,
                    cache_read,
                    cache_write,
                    now
                ])
                .is_err()
            {
                skipped.push(model_id.to_owned());
                continue;
            }
            written += 1;
        }
    }
    if let Err(error) = transaction.commit() {
        eprintln!("提交失败：{error}");
        std::process::exit(1);
    }

    println!("价格表已更新：{written} 个模型，跳过 {} 个", skipped.len());
    if purged_prices > 0 || cleared_costs > 0 {
        println!("清除无效价格 {purged_prices} 条，重置无效计价事件 {cleared_costs} 条");
    }
    if !skipped.is_empty() {
        println!("跳过清单：{}", skipped.join(", "));
    }

    match cost::PriceTable::load(&connection) {
        Ok(price_table) => match cost::reprice_unpriced_events(&mut connection, &price_table) {
            Ok(count) => println!("重算历史未计价事件：{count} 条"),
            Err(e) => eprintln!("重算历史事件失败：{e}"),
        },
        Err(e) => eprintln!("加载价格表失败：{e}"),
    }

    // Reminder outlet 3 (plan §6.4): the caller sees which models still need a
    // price or an explicit `ignore` while the fetch is fresh in mind.
    let since = now - cost::WINDOW_7D_MS;
    match cost::unresolved_within(&connection, since) {
        Ok(rows) if rows.is_empty() => println!("近 7 天无未计价模型。"),
        Ok(rows) => {
            println!("近 7 天未计价模型（按命中次数降序）：");
            for row in rows {
                println!(
                    "  {:>7} 次  {:<8} {}",
                    row.hit_count, row.source, row.raw_model
                );
            }
        }
        Err(error) => eprintln!("读取未计价清单失败：{error}"),
    }
}

/// Download the catalogue. OpenRouter needs no authentication for this endpoint.
fn fetch() -> Result<String, Box<dyn std::error::Error>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("herdr-usage/import_prices")
        .timeout(std::time::Duration::from_secs(60))
        .build()?;
    let response = client.get(ENDPOINT).send()?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()).into());
    }
    Ok(response.text()?)
}

/// Read one price. Every price in the payload is a JSON **string** (e.g.
/// `"0.00001"`), so reading it as a number would silently yield 0.
fn price(pricing: &Value, key: &str) -> Option<f64> {
    let value = pricing.get(key)?;
    if value.is_null() {
        return None;
    }
    let parsed = match value {
        Value::String(text) => text.trim().parse::<f64>().ok()?,
        Value::Number(number) => number.as_f64()?,
        _ => return None,
    };
    // `-1` is OpenRouter's sentinel for dynamic/unknown pricing (routers and
    // switchyards), not a real fee. Billing it makes daily totals negative.
    if parsed.is_finite() && parsed >= 0.0 {
        Some(parsed)
    } else {
        None
    }
}

/// Remove any stored negative price and reset events billed with it.
/// Returns (price rows removed, events reset).
fn purge_invalid_prices(
    transaction: &rusqlite::Transaction<'_>,
) -> rusqlite::Result<(usize, usize)> {
    // Resolve before deleting prices, using the same alias and fallback rules
    // as billing. A negative component can still produce a nonnegative total.
    let table = cost::PriceTable::load(transaction)?;
    let models: Vec<String> = {
        let mut statement = transaction.prepare(
            "SELECT DISTINCT model FROM usage_event WHERE model IS NOT NULL AND cost_usd IS NOT NULL",
        )?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut events = 0;
    for model in models {
        if table.has_negative_price(&model) {
            events += transaction.execute(
                "UPDATE usage_event SET cost_usd = NULL WHERE model = ?1 AND cost_usd IS NOT NULL",
                [model],
            )?;
        }
    }
    // Also repair negative amounts left behind by earlier price-table edits.
    events += transaction.execute(
        "UPDATE usage_event SET cost_usd = NULL WHERE cost_usd < 0",
        [],
    )?;
    let prices = transaction.execute(
        "DELETE FROM model_price
         WHERE prompt < 0 OR completion < 0
            OR IFNULL(cache_read, 0) < 0 OR IFNULL(cache_write, 0) < 0",
        [],
    )?;
    if events > 0 {
        db::refresh_hourly_costs(transaction)?;
    }
    Ok((prices, events))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prices_are_decimal_strings_and_must_be_parsed() {
        let pricing = json!({"prompt": "0.00000175", "completion": "0.000014"});
        assert_eq!(price(&pricing, "prompt"), Some(0.00000175));
        assert_eq!(price(&pricing, "completion"), Some(0.000014));
        // The trap this guards: a string that never reaches the numeric path.
        assert_ne!(price(&pricing, "prompt"), Some(0.0));
    }

    #[test]
    fn a_missing_key_is_none_rather_than_zero() {
        let pricing = json!({"prompt": "0.1", "input_cache_read": null});
        assert_eq!(price(&pricing, "completion"), None);
        assert_eq!(price(&pricing, "input_cache_read"), None);
        // Most models have no input_cache_write key at all.
        assert_eq!(price(&pricing, "input_cache_write"), None);
    }

    #[test]
    fn a_malformed_price_is_rejected_instead_of_billed_as_free() {
        let pricing = json!({"prompt": "free", "completion": ""});
        assert_eq!(price(&pricing, "prompt"), None);
        assert_eq!(price(&pricing, "completion"), None);
    }

    #[test]
    fn numbers_are_accepted_for_forward_compatibility() {
        let pricing = json!({"prompt": 0.5});
        assert_eq!(price(&pricing, "prompt"), Some(0.5));
    }

    #[test]
    fn negative_sentinel_prices_are_rejected_but_free_models_are_kept() {
        let sentinel = json!({"prompt": "-1", "completion": "-1"});
        assert_eq!(price(&sentinel, "prompt"), None);
        assert_eq!(price(&sentinel, "completion"), None);
        let free = json!({"prompt": "0", "completion": "0"});
        assert_eq!(price(&free, "prompt"), Some(0.0));
        assert_eq!(price(&free, "completion"), Some(0.0));
    }

    #[test]
    fn purge_removes_negative_price_rows_and_resets_their_events() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        db::initialize(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO model_price(model_id, prompt, completion, cache_read, cache_write, updated_at)
                 VALUES ('openrouter/auto', -1, -1, NULL, NULL, 0)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO usage_event(source, event_id, model, input_total, cache_read,
                     cache_write, output_total, reasoning, cost_usd, occurred_at)
                 VALUES ('omp', 'bad', 'openrouter/auto', 100, 0, 0, 10, 0, -100, 0)",
                [],
            )
            .unwrap();

        let transaction = connection.transaction().unwrap();
        assert_eq!(purge_invalid_prices(&transaction).unwrap(), (1, 1));
        transaction.commit().unwrap();

        let prices: i64 = connection
            .query_row("SELECT COUNT(*) FROM model_price", [], |row| row.get(0))
            .unwrap();
        assert_eq!(prices, 0);
        let cost: Option<f64> = connection
            .query_row(
                "SELECT cost_usd FROM usage_event WHERE event_id = 'bad'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cost, None);
    }

    fn maintenance_fixture() -> (rusqlite::Connection, i64) {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        db::initialize(&connection).unwrap();
        connection.execute_batch(
            "INSERT INTO model_price(model_id, prompt, completion, cache_read, cache_write, updated_at) VALUES
                 ('vendor/bad', -1, 2, NULL, NULL, 0),
                 ('vendor/cache', 1, 1, -1, NULL, 0),
                 ('vendor/write', 1, 1, NULL, -1, 0),
                 ('vendor/output', 1, -1, NULL, NULL, 0),
                 ('vendor/good', 1, 1, NULL, NULL, 0),
                 ('other/bad', 1, 1, NULL, NULL, 0);
             INSERT INTO model_alias(raw_model, model_id, ignore, resolved_by) VALUES
                 ('alias', 'vendor/bad', 0, 'manual'),
                 ('ignored', 'vendor/bad', 1, 'ignore');"
        ).unwrap();
        let now = 1_700_000_000_000;
        let start = db::compute_current_hour_start_ms(now).unwrap();
        for (id, model, amount, at) in [
            ("negative", "vendor/bad", -1.0, start - 3_600_000),
            ("zero", "vendor/bad", 0.0, start - 3_600_000),
            ("positive", "alias", 1.0, start - 3_600_000),
            ("cache", "client/cache", 1.0, start - 3_600_000),
            ("write", "write", 1.0, start - 3_600_000),
            ("output", "vendor/output", 1.0, start - 3_600_000),
            ("good", "vendor/good", 5.0, start - 3_600_000),
            ("exact", "other/bad", 4.0, start - 3_600_000),
            ("ignored", "ignored", 3.0, start - 3_600_000),
            ("ambiguous", "client/bad", 2.0, start - 3_600_000),
            ("unknown", "unknown", -2.0, start - 7_200_000),
            ("current", "vendor/bad", 1.0, start),
        ] {
            connection
                .execute(
                    "INSERT INTO usage_event(source, event_id, model, input_total, cache_read,
                     cache_write, output_total, reasoning, cost_usd, occurred_at)
                 VALUES ('omp', ?1, ?2, 1, 0, 0, 1, 0, ?3, ?4)",
                    rusqlite::params![id, model, amount, at],
                )
                .unwrap();
        }
        db::rollup_closed_hours(&mut connection, now).unwrap();
        (connection, now)
    }

    fn event_cost(connection: &rusqlite::Connection, id: &str) -> Option<f64> {
        connection
            .query_row(
                "SELECT cost_usd FROM usage_event WHERE event_id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn purge_clears_all_affected_costs_and_refreshes_archives_then_reprices() {
        let (mut connection, now) = maintenance_fixture();
        let cursor = db::offset(&connection, db::HOURLY_ROLLUP_KIND).unwrap();
        let tx = connection.transaction().unwrap();
        assert_eq!(purge_invalid_prices(&tx).unwrap(), (4, 8));
        tx.commit().unwrap();
        for id in [
            "negative", "zero", "positive", "cache", "write", "output", "unknown", "current",
        ] {
            assert_eq!(event_cost(&connection, id), None, "{id}");
        }
        for (id, amount) in [
            ("good", 5.0),
            ("exact", 4.0),
            ("ignored", 3.0),
            ("ambiguous", 2.0),
        ] {
            assert_eq!(event_cost(&connection, id), Some(amount), "{id}");
        }
        let archived: (f64, i64, i64) = connection
            .query_row(
                "SELECT SUM(cost_usd), SUM(events), SUM(tokens) FROM usage_hourly WHERE model = ''",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(archived, (14.0, 11, 22));
        let invalid_model_cost: Option<f64> = connection
            .query_row(
                "SELECT cost_usd FROM usage_hourly WHERE model = 'vendor/bad'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(invalid_model_cost, Some(3.0)); // The ignored alias retains its existing charge.
        let cache_cost: Option<f64> = connection
            .query_row(
                "SELECT cost_usd FROM usage_hourly WHERE model = 'client/cache'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cache_cost, None);
        let unknown_hour: Option<f64> = connection.query_row(
            "SELECT cost_usd FROM usage_hourly WHERE model = '__unknown__' OR model = 'unknown'",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(unknown_hour, None);
        assert_eq!(
            db::offset(&connection, db::HOURLY_ROLLUP_KIND).unwrap(),
            cursor
        );
        assert_eq!(db::rollup_closed_hours(&mut connection, now).unwrap(), 0);

        // A valid replacement must repair archived costs as well as raw events.
        connection
            .execute(
                "INSERT INTO model_price(model_id, prompt, completion, updated_at)
             VALUES ('vendor/bad', 1, 1, 0)",
                [],
            )
            .unwrap();
        let table = cost::PriceTable::load(&connection).unwrap();
        assert_eq!(
            cost::reprice_unpriced_events(&mut connection, &table).unwrap(),
            4
        );
        let total: f64 = connection
            .query_row(
                "SELECT SUM(cost_usd) FROM usage_hourly WHERE model = ''",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(total, 20.0); // Three archived events, one current event.
        let model_total: f64 = connection
            .query_row(
                "SELECT cost_usd FROM usage_hourly WHERE model = 'vendor/bad'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(model_total, 9.0);
        assert_eq!(
            cost::reprice_unpriced_events(&mut connection, &table).unwrap(),
            0
        );
        let tx = connection.transaction().unwrap();
        assert_eq!(purge_invalid_prices(&tx).unwrap(), (0, 0));
        tx.commit().unwrap();
    }

    #[test]
    fn archive_refresh_failure_rolls_back_price_and_event_cleanup() {
        let (mut connection, _) = maintenance_fixture();
        connection
            .execute_batch(
                "CREATE TRIGGER fail_refresh BEFORE UPDATE ON usage_hourly
             BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
            )
            .unwrap();
        let tx = connection.transaction().unwrap();
        assert!(purge_invalid_prices(&tx).is_err());
        drop(tx);
        assert_eq!(event_cost(&connection, "negative"), Some(-1.0));
        assert_eq!(event_cost(&connection, "positive"), Some(1.0));
        let prices: i64 = connection
            .query_row("SELECT COUNT(*) FROM model_price", [], |row| row.get(0))
            .unwrap();
        assert_eq!(prices, 6);
        connection
            .execute_batch("DROP TRIGGER fail_refresh")
            .unwrap();
        let tx = connection.transaction().unwrap();
        purge_invalid_prices(&tx).unwrap();
        tx.commit().unwrap();
        connection
            .execute_batch(
                "INSERT INTO model_price(model_id, prompt, completion, updated_at)
             VALUES ('vendor/bad', 1, 1, 0);
             CREATE TRIGGER fail_refresh BEFORE UPDATE ON usage_hourly
             BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
            )
            .unwrap();
        let table = cost::PriceTable::load(&connection).unwrap();
        assert!(cost::reprice_unpriced_events(&mut connection, &table).is_err());
        assert_eq!(event_cost(&connection, "positive"), None);
        let total: f64 = connection
            .query_row(
                "SELECT SUM(cost_usd) FROM usage_hourly WHERE model = ''",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(total, 14.0);
    }
}
