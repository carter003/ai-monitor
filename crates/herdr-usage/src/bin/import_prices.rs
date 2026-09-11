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
    parsed.is_finite().then_some(parsed)
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
}
