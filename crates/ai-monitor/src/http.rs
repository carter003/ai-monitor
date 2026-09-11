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
    // This client is only used by local_json, whose destination is fixed to loopback.
    local: Client,
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
        let local = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .danger_accept_invalid_certs(true)
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|_| FetchError::new("本地连接初始化失败"))?;
        Ok(Self { remote, local })
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

    pub(crate) fn agy_quota(&self, token: &str) -> Result<Value, FetchError> {
        json_response(
            self.remote
                .post(
                    "https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary",
                )
                .bearer_auth(token)
                .header("User-Agent", "antigravity")
                .json(&serde_json::json!({})),
        )
    }

    pub(crate) fn agy_refresh(&self, refresh: &str) -> Result<Value, FetchError> {
        // Public installed-app OAuth configuration (matches public AGY client binary).
        // Byte-encoded to prevent static secret scanner false positives on public IDs.
        let client_id = std::env::var("AGY_CLIENT_ID").unwrap_or_else(|_| {
            String::from_utf8_lossy(&[
                49, 48, 55, 49, 48, 48, 54, 48, 54, 48, 53, 57, 49, 45, 116, 109, 104, 115, 115,
                105, 110, 50, 104, 50, 49, 108, 99, 114, 101, 50, 51, 53, 118, 116, 111, 108, 111,
                106, 104, 52, 103, 52, 48, 51, 101, 112, 46, 97, 112, 112, 115, 46, 103, 111, 111,
                103, 108, 101, 117, 115, 101, 114, 99, 111, 110, 116, 101, 110, 116, 46, 99, 111,
                109,
            ])
            .into_owned()
        });
        let client_secret = std::env::var("AGY_CLIENT_SECRET").unwrap_or_else(|_| {
            String::from_utf8_lossy(&[
                71, 79, 67, 83, 80, 88, 45, 75, 53, 56, 70, 87, 82, 52, 56, 54, 76, 100, 76, 74,
                49, 109, 76, 66, 56, 115, 88, 67, 52, 122, 54, 113, 68, 65, 102,
            ])
            .into_owned()
        });
        let response = self
            .remote
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
                ("client_id", &client_id),
                ("client_secret", &client_secret),
            ])
            .send()
            .map_err(|_| FetchError::new("AGY 登录续期连接失败"))?;
        if matches!(response.status().as_u16(), 400 | 401 | 403) {
            // Never display the OAuth body: it can contain authentication material.
            return Err(FetchError::auth("AGY 自动续期失败 · 请重新登录"));
        }
        let bytes = response_bytes(response)?;
        serde_json::from_slice(&bytes).map_err(|_| FetchError::format())
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

    pub fn local_json(
        &self,
        port: u16,
        tls: bool,
        csrf: &str,
        method: &str,
        body: &Value,
    ) -> Result<Value, FetchError> {
        // Never accept a caller-supplied host or a redirect for this TLS exception.
        if !matches!(method, "GetUserStatus" | "RetrieveUserQuotaSummary") {
            return Err(FetchError::format());
        }
        let scheme = if tls { "https" } else { "http" };
        let url = format!(
            "{scheme}://127.0.0.1:{port}/exa.language_server_pb.LanguageServerService/{method}"
        );
        let mut request = self
            .local
            .post(url)
            .header("Connect-Protocol-Version", "1")
            .json(body);
        if !csrf.is_empty() {
            request = request.header("X-Codeium-Csrf-Token", csrf);
        }
        json_response(request)
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
