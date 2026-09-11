//! The Grok Build billing endpoint's unary gRPC-web subscription response.
//! Decode specific fields; never infer quota from an arbitrary float in a bill.
use crate::model::{Card, FetchError, Meter, window_label};

pub fn parse(bytes: &[u8]) -> Result<Vec<Card>, FetchError> {
    let payload = data_frame(bytes)?;
    let config = fields(payload)?
        .into_iter()
        .find_map(|(id, field)| match (id, field) {
            (1, Field::Bytes(body)) => Some(body),
            _ => None,
        })
        .ok_or_else(FetchError::format)?;
    let (mut used, mut start, mut end) = (None, None, None);
    for (id, field) in fields(config)? {
        match (id, field) {
            (1, Field::Fixed32(value)) => used = Some(f32::from_le_bytes(value) as f64),
            (4, Field::Bytes(value)) => start = Some(proto_time(value)?),
            (5, Field::Bytes(value)) => end = Some(proto_time(value)?),
            _ => (),
        }
    }
    let start = start.ok_or_else(FetchError::format)?;
    let end = end.ok_or_else(FetchError::format)?;
    let duration = end.checked_sub(start).ok_or_else(FetchError::format)?;
    if !(86400..=31 * 86400).contains(&duration) {
        return Err(FetchError::format());
    }
    // Proto3 omits a scalar at its default value. Only accept this after the
    // full subscription period has been validated, never for an empty response.
    let used = used.unwrap_or(0.);
    if !used.is_finite() || !(0.0..=100.0).contains(&used) {
        return Err(FetchError::format());
    }
    Ok(vec![Card {
        // 二进制 protobuf 没有官方文本小数位，保持整数显示。
        meters: vec![Meter::from_used(
            window_label(duration),
            used,
            Some(end),
            0,
        )?],
        ..Card::empty("SuperGrok")
    }])
}

fn data_frame(bytes: &[u8]) -> Result<&[u8], FetchError> {
    let mut pos = 0usize;
    let mut data = None;
    while pos < bytes.len() {
        let header = bytes.get(pos..pos + 5).ok_or_else(FetchError::format)?;
        pos += 5;
        let length =
            u32::from_be_bytes(header[1..5].try_into().map_err(|_| FetchError::format())?) as usize;
        let end = pos.checked_add(length).ok_or_else(FetchError::format)?;
        let body = bytes.get(pos..end).ok_or_else(FetchError::format)?;
        pos = end;
        match header[0] {
            0 if data.is_none() => data = Some(body),
            128 => {
                for line in body.split(|b| *b == b'\n') {
                    let text = String::from_utf8_lossy(line);
                    if let Some((key, value)) = text.split_once(':')
                        && key.trim().eq_ignore_ascii_case("grpc-status")
                    {
                        match value.trim() {
                            "0" => (),
                            "16" => return Err(FetchError::auth("Grok 登录已过期")),
                            "7" => return Err(FetchError::new("Grok 无额度查询权限")),
                            _ => return Err(FetchError::new("Grok 额度服务暂不可用")),
                        }
                    }
                }
            }
            _ => return Err(FetchError::format()),
        }
    }
    data.filter(|b| !b.is_empty())
        .ok_or_else(|| FetchError::new("Grok 未返回订阅额度"))
}

enum Field<'a> {
    Varint(u64),
    Fixed32([u8; 4]),
    Bytes(&'a [u8]),
    Other,
}

fn varint(bytes: &[u8], pos: &mut usize) -> Result<u64, FetchError> {
    let mut result = 0;
    for shift in (0..70).step_by(7) {
        let byte = *bytes.get(*pos).ok_or_else(FetchError::format)?;
        *pos += 1;
        if shift == 63 && byte > 1 {
            return Err(FetchError::format());
        }
        result |= u64::from(byte & 127) << shift;
        if byte & 128 == 0 {
            return Ok(result);
        }
    }
    Err(FetchError::format())
}

fn fields(bytes: &[u8]) -> Result<Vec<(u64, Field<'_>)>, FetchError> {
    let mut result = vec![];
    let mut pos = 0;
    while pos < bytes.len() {
        let tag = varint(bytes, &mut pos)?;
        let id = tag >> 3;
        if id == 0 {
            return Err(FetchError::format());
        }
        let field = match tag & 7 {
            0 => Field::Varint(varint(bytes, &mut pos)?),
            2 => {
                let length =
                    usize::try_from(varint(bytes, &mut pos)?).map_err(|_| FetchError::format())?;
                let end = pos.checked_add(length).ok_or_else(FetchError::format)?;
                let body = bytes.get(pos..end).ok_or_else(FetchError::format)?;
                pos = end;
                Field::Bytes(body)
            }
            5 => {
                let field = bytes.get(pos..pos + 4).ok_or_else(FetchError::format)?;
                pos += 4;
                Field::Fixed32(field.try_into().map_err(|_| FetchError::format())?)
            }
            1 => {
                bytes.get(pos..pos + 8).ok_or_else(FetchError::format)?;
                pos += 8;
                Field::Other
            }
            _ => return Err(FetchError::format()),
        };
        result.push((id, field));
    }
    Ok(result)
}

fn proto_time(bytes: &[u8]) -> Result<i64, FetchError> {
    let mut time = None;
    for (id, field) in fields(bytes)? {
        match (id, field) {
            (1, Field::Varint(t)) if t > 0 => time = i64::try_from(t).ok(),
            (2, Field::Varint(nanos)) if nanos >= 1_000_000_000 => return Err(FetchError::format()),
            _ => (),
        }
    }
    time.ok_or_else(FetchError::format)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn encode_varint(mut value: u64) -> Vec<u8> {
        let mut bytes = vec![];
        while value >= 128 {
            bytes.push((value as u8 & 127) | 128);
            value >>= 7;
        }
        bytes.push(value as u8);
        bytes
    }
    fn message(id: u8, body: Vec<u8>) -> Vec<u8> {
        let mut result = vec![(id << 3) | 2];
        result.extend(encode_varint(body.len() as u64));
        result.extend(body);
        result
    }
    fn frame(flag: u8, body: Vec<u8>) -> Vec<u8> {
        let mut result = vec![flag];
        result.extend((body.len() as u32).to_be_bytes());
        result.extend(body);
        result
    }
    fn sample(used: Option<f32>) -> Vec<u8> {
        let mut config = vec![];
        if let Some(used) = used {
            config.push(13);
            config.extend(used.to_le_bytes());
        }
        for (field, time) in [(4, 1800000000u64), (5, 1800604800u64)] {
            let mut timestamp = vec![8];
            timestamp.extend(encode_varint(time));
            config.extend(message(field, timestamp));
        }
        frame(0, message(1, config))
    }
    #[test]
    fn decodes_subscription_period_and_used_percentage() {
        let cards = parse(&sample(Some(27.5))).unwrap();
        assert_eq!(cards[0].meters[0].remaining, Some(72.5));
        assert_eq!(cards[0].meters[0].label, "周");
    }
    #[test]
    fn zero_usage_requires_an_actual_valid_subscription() {
        assert_eq!(
            parse(&sample(None)).unwrap()[0].meters[0].remaining,
            Some(100.)
        );
        assert!(parse(&[]).is_err());
        assert!(parse(&frame(0, message(1, vec![]))).is_err());
    }
    #[test]
    fn grpc_errors_and_truncation_cannot_be_treated_as_success() {
        let mut input = sample(Some(20.));
        input.extend(frame(128, b"grpc-status:16\r\n".to_vec()));
        assert!(parse(&input).unwrap_err().invalidate);
        let valid = sample(Some(20.));
        for cut in 0..valid.len() {
            assert!(parse(&valid[..cut]).is_err());
        }
    }
}
