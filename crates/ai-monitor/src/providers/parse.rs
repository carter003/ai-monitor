use crate::model::{Card, FetchError, Meter, window_label};
use serde_json::Value;

pub fn timestamp(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| {
            let s = value.as_str()?;
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|t| t.timestamp())
                .or_else(|| s.parse().ok())
        })
        .or_else(|| value.get("seconds").and_then(timestamp))
}

pub fn codex(value: &Value) -> Result<Vec<Card>, FetchError> {
    let mut cards = vec![codex_card(
        "Codex · GPT",
        value.get("rate_limit").ok_or_else(FetchError::format)?,
    )?];
    if let Some(extras) = value
        .get("additional_rate_limits")
        .and_then(Value::as_array)
    {
        for entry in extras {
            let name = entry
                .get("limit_name")
                .and_then(Value::as_str)
                .unwrap_or("独立额度");
            // GPT-5.3-Codex-Spark 已停用：旧响应里可能还带着这个池，一律不统计。
            if entry.get("metered_feature").and_then(Value::as_str) == Some("codex_bengalfox")
                || name.to_ascii_lowercase().contains("spark")
            {
                continue;
            }
            cards.push(codex_card(
                &format!("Codex · {}", safe_label(name)),
                entry.get("rate_limit").ok_or_else(FetchError::format)?,
            )?);
        }
    }
    Ok(cards)
}

fn codex_card(title: &str, value: &Value) -> Result<Card, FetchError> {
    let mut card = Card::empty(title);
    for key in ["primary_window", "secondary_window"] {
        if let Some(window) = value.get(key).filter(|v| !v.is_null()) {
            let used_value = window.get("used_percent").ok_or_else(FetchError::format)?;
            let used = used_value.as_f64().ok_or_else(FetchError::format)?;
            let seconds = window
                .get("limit_window_seconds")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            card.meters.push(Meter::from_used(
                window_label(seconds),
                used,
                window.get("reset_at").and_then(timestamp),
                decimals(used_value),
            )?);
        }
    }
    if card.meters.is_empty() {
        return Err(FetchError::format());
    }
    Ok(card)
}

pub fn go(value: &Value, title: &str) -> Result<Vec<Card>, FetchError> {
    let usage = value.get("usage").ok_or_else(FetchError::format)?;
    let mut card = Card::empty(title);
    for (key, label) in [("rolling", "5H"), ("weekly", "周"), ("monthly", "月")] {
        let window = usage.get(key).ok_or_else(FetchError::format)?;
        let used_value = window.get("percent").ok_or_else(FetchError::format)?;
        let used = used_value.as_f64().ok_or_else(FetchError::format)?;
        card.meters.push(Meter::from_used(
            label,
            used,
            window.get("resetsAt").and_then(timestamp),
            // 官方给整数就按整数显示，不凑小数位；上限由 from_used 统一收敛。
            decimals(used_value),
        )?);
    }
    Ok(vec![card])
}

pub fn openrouter(value: &Value) -> Result<Vec<Card>, FetchError> {
    let data = value.get("data").ok_or_else(FetchError::format)?;
    let credits = data
        .get("total_credits")
        .and_then(Value::as_f64)
        .ok_or_else(FetchError::format)?;
    let usage = data
        .get("total_usage")
        .and_then(Value::as_f64)
        .ok_or_else(FetchError::format)?;
    let balance = credits - usage;
    if !balance.is_finite() || credits < 0. || usage < 0. {
        return Err(FetchError::format());
    }
    Ok(vec![Card {
        balance: Some(balance),
        ..Card::empty("OpenRouter")
    }])
}

fn safe_label(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).take(45).collect()
}

/// 官方 JSON 数字自带的小数位数；整数返回 0。调用方按剩余值换算后限位。
fn decimals(value: &Value) -> u8 {
    // serde_json 的整数永远不是 f64，一个分支即覆盖整数与非数字。
    if !value.is_f64() {
        return 0;
    }
    let text = value.to_string();
    let Some(dot) = text.find('.') else {
        return 0;
    };
    let frac: String = text[dot + 1..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    frac.len().min(u8::MAX as usize) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn retired_spark_pool_is_never_reported() {
        let w = |seconds, used| json!({"limit_window_seconds":seconds,"used_percent":used,"reset_at":1800000000});
        let gpt = json!({"primary_window":w(604800,43),"secondary_window":null});
        let spark = json!({"metered_feature":"codex_bengalfox","limit_name":"GPT-5.3-Codex-Spark","rate_limit":{"primary_window":w(18000,0),"secondary_window":w(604800,18)}});
        let cards =
            codex(&json!({"rate_limit":gpt.clone(),"additional_rate_limits":[spark]})).unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].title, "Codex · GPT");
        assert_eq!(cards[0].meters.len(), 1);
        assert_eq!(cards[0].meters[0].label, "周");
        assert_eq!(cards[0].meters[0].remaining, Some(57.));
        // 账户没有独立额度池时也不留占位卡。
        assert_eq!(codex(&json!({"rate_limit":gpt})).unwrap().len(), 1);
    }
    #[test]
    fn other_codex_pools_keep_their_windows() {
        let w = |seconds, used| json!({"limit_window_seconds":seconds,"used_percent":used,"reset_at":1800000000});
        let value = json!({"rate_limit":{"primary_window":w(604800,43),"secondary_window":null},"additional_rate_limits":[{"metered_feature":"codex_other","limit_name":"GPT-5.4 mini","rate_limit":{"primary_window":w(18000,25),"secondary_window":null}}]});
        let cards = codex(&value).unwrap();
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[1].title, "Codex · GPT-5.4 mini");
        assert_eq!(cards[1].meters[0].label, "5H");
        assert_eq!(cards[1].meters[0].remaining, Some(75.));
    }
    #[test]
    fn go_shows_remaining_and_never_uses_a_local_billing_estimate() {
        let w = |p| json!({"percent":p,"resetsAt":"2026-09-08T05:42:58Z"});
        let cards = go(
            &json!({"usage":{"rolling":w(0),"weekly":w(31),"monthly":w(98)}}),
            "OpenCode Go",
        )
        .unwrap();
        assert_eq!(cards[0].meters[2].remaining, Some(2.));
        assert!(go(&json!({"usage":{"monthly":w(98)}}), "OpenCode Go").is_err());
    }
    #[test]
    fn go_keeps_official_precision_without_padding_integers() {
        let value = json!({"usage":{
            "rolling": {"percent": 12.5, "resetsAt": "2026-09-08T05:42:58Z"},
            "weekly": {"percent": 31, "resetsAt": "2026-09-15T05:42:58Z"},
            "monthly": {"percent": 0, "resetsAt": "2026-10-01T05:42:58Z"},
        }});
        let cards = go(&value, "OpenCode Go").unwrap();
        assert!((cards[0].meters[0].remaining.unwrap() - 87.5).abs() < 0.001);
        assert_eq!(cards[0].meters[0].decimals, 1);
        assert_eq!(cards[0].meters[1].decimals, 0);
        assert_eq!(cards[0].meters[2].remaining, Some(100.));
        assert_eq!(cards[0].meters[2].decimals, 0);
    }
    #[test]
    fn go_renders_either_card_title() {
        let w = |p| json!({"percent":p,"resetsAt":"2026-09-08T05:42:58Z"});
        for title in ["OpenCode Go", "OpenCode GO-2"] {
            let cards = go(
                &json!({"usage":{"rolling":w(0),"weekly":w(0),"monthly":w(0)}}),
                title,
            )
            .unwrap();
            assert_eq!(cards[0].title, title);
        }
    }
    fn openrouter_is_account_credit_less_account_usage() {
        let card =
            openrouter(&json!({"data":{"total_credits":100.5,"total_usage":25.75}})).unwrap();
        assert_eq!(card[0].balance, Some(74.75));
        assert!(openrouter(&json!({"data":{"limit_remaining":123}})).is_err());
    }
}
