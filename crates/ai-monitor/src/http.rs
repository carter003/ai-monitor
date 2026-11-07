use crate::model::FetchError;
use reqwest::{
    blocking::{Client, RequestBuilder, Response},
    redirect::Policy,
};
use serde_json::Value;
use std::{io::Read, time::Duration};

const MAX_RESPONSE: u64 = 2 * 1024 * 1024;

#[derive(Clone)]
pub struct Http {
    remote: Client,
}

impl Http {
    pub fn new() -> Result<Self, FetchError> {
        let remote = Client::builder()
            .user_agent(concat!("ai-monitor/", env!("CARGO_PKG_VERSION")))
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(12))
            .build()
            .map_err(|_| FetchError::new("网络客户端初始化失败"))?;
        Ok(Self { remote })
    }

    pub fn get(
        &self,
        url: &'static str,
        token: &str,
        account: Option<&str>,
    ) -> Result<Value, FetchError> {
        let mut request = self.remote.get(url).bearer_auth(token);
        if let Some(account) = account {
            request = request.header("ChatGPT-Account-Id", account);
        }
        json_response(request)
    }

    pub fn grok(&self, token: &str) -> Result<Vec<u8>, FetchError> {
        let response = self
            .remote
            .post("https://grok.com/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig")
            .bearer_auth(token)
            .header("Content-Type", "application/grpc-web+proto")
            .header("x-grpc-web", "1")
            .header("x-user-agent", "connect-es/2.1.1")
            .header("Origin", "https://grok.com")
            .header("Referer", "https://grok.com/")
            .body(vec![0; 5])
            .send()
            .map_err(|_| FetchError::new("Grok 连接失败"))?;
        response_bytes(response)
    }
}

fn json_response(request: RequestBuilder) -> Result<Value, FetchError> {
    let response = request.send().map_err(|e| {
        if e.is_timeout() {
            FetchError::new("连接超时")
        } else {
            FetchError::new("连接失败")
        }
    })?;
    let bytes = response_bytes(response)?;
    serde_json::from_slice(&bytes).map_err(|_| FetchError::new("额度接口格式已变化"))
}

fn response_bytes(mut response: Response) -> Result<Vec<u8>, FetchError> {
    let status = response.status().as_u16();
    if status != 200 {
        let mut error = match status {
            401 => FetchError::auth("登录或凭据失效 · 401"),
            403 => FetchError::new("无查询权限 · 403"),
            429 => {
                let mut error = FetchError::new("查询受限 · 稍后重试");
                error.retry_after = Some(Duration::from_secs(60));
                error
            }
            300..=399 => FetchError::new("需要重新登录"),
            _ => FetchError::new(format!("服务暂不可用 · {status}")),
        };
        if let Some(value) = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
        {
            let seconds = value.parse::<u64>().ok().or_else(|| {
                chrono::DateTime::parse_from_rfc2822(value)
                    .ok()
                    .map(|t| (t.timestamp() - chrono::Utc::now().timestamp()).max(1) as u64)
            });
            // Keep malformed/absurd headers from overflowing Instant arithmetic.
            error.retry_after = seconds
                .map(|s| Duration::from_secs(s.clamp(5, 365 * 86400)))
                .or(error.retry_after);
        }
        return Err(error);
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(MAX_RESPONSE + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| FetchError::new("响应读取失败"))?;
    if bytes.len() as u64 > MAX_RESPONSE {
        return Err(FetchError::new("响应过大"));
    }
    if bytes.is_empty() {
        return Err(FetchError::new("服务未返回额度"));
    }
    Ok(bytes)
}
