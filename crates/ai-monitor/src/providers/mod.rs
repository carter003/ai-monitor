mod agy;
mod agy_cloud;
mod grok;
mod parse;

use crate::{
    config::Config,
    http::Http,
    model::{Card, FetchError, Source},
};
use serde_json::Value;
use std::{
    env,
    fs::File,
    hash::{DefaultHasher, Hash, Hasher},
    io::Read,
    path::{Path, PathBuf},
};

// Deliberately no Debug: this object contains authentication material.
pub struct Prepared {
    pub identity: String,
    token: String,
    account: Option<String>,
    profile: Option<PathBuf>,
    expires: Option<i64>,
    agy_credentials: Option<agy_cloud::Credentials>,
}

pub fn prepare(source: Source, config: &Config) -> Result<Prepared, FetchError> {
    let mut input = Prepared {
        identity: String::new(),
        token: String::new(),
        account: None,
        profile: None,
        expires: None,
        agy_credentials: None,
    };
    match source {
        Source::Codex => {
            let auth = read_json(&config.codex.join("auth.json"))?;
            input.token = auth
                .pointer("/tokens/access_token")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into();
            input.account = auth
                .pointer("/tokens/account_id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if input.token.is_empty() {
                return Err(FetchError::auth("请先登录 Codex 订阅账户"));
            }
        }
        Source::Agy | Source::Agy2 => {
            let root = if source == Source::Agy {
                &config.agy
            } else {
                &config.agy2
            };
            if !root.is_dir() {
                return Err(FetchError::auth("未找到此 AGY 账户"));
            }
            input.agy_credentials = agy_cloud::Credentials::load(root)?;
            input.token = input
                .agy_credentials
                .as_ref()
                .map(agy_cloud::Credentials::identity)
                .unwrap_or_default();
            input.profile = Some(root.clone());
            input.account = Some(root.to_string_lossy().into_owned());
        }
        Source::Go => {
            let auth = read_json(&config.opencode.join("auth.json"))?;
            input.token = auth
                .pointer("/opencode-go/key")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into();
            if input.token.is_empty() {
                return Err(FetchError::auth("请先连接 OpenCode Go"));
            }
        }
        Source::Go2 => {
            if !config.go2_key.exists() {
                return Err(FetchError::auth("请先写入 OpenCode GO-2 Key 文件"));
            }
            input.token = read_text(&config.go2_key)?.trim().into();
            if input.token.is_empty() {
                return Err(FetchError::auth("OpenCode GO-2 Key 文件为空"));
            }
        }
        Source::Grok => {
            let auth = read_json(&config.grok.join("auth.json"))?;
            let auth = auth
                .as_object()
                .and_then(|map| {
                    map.iter()
                        .find(|(key, _)| key.starts_with("https://auth.x.ai"))
                })
                .map(|(_, auth)| auth)
                .ok_or_else(|| FetchError::auth("请先登录 SuperGrok"))?;
            input.token = auth
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into();
            input.account = auth
                .get("user_id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            input.expires = auth.get("expires_at").and_then(parse::timestamp);
            if input.token.is_empty() {
                return Err(FetchError::auth("请先登录 SuperGrok"));
            }
            if input
                .expires
                .is_some_and(|t| t <= chrono::Utc::now().timestamp())
            {
                return Err(FetchError::auth("登录已过期 · 请登录 Grok"));
            }
        }
        Source::OpenRouter => {
            if config.openrouter_key.exists() {
                input.token = read_text(&config.openrouter_key)?.trim().into();
            } else if let Some(key) = env::var("OPENROUTER_MANAGEMENT_KEY")
                .ok()
                .or_else(|| env::var("OPENROUTER_API_KEY").ok())
            {
                input.token = key.trim().into();
            } else {
                input.token = opencode_key(&config.opencode, "openrouter")?;
            }
            if input.token.is_empty() {
                return Err(FetchError::auth("未配置余额查询凭据"));
            }
        }
    }
    // Account ID when available, credential fingerprint otherwise. Include profile
    // roots so the two AGY accounts never share a state slot.
    input.identity = fingerprint(input.account.as_deref().unwrap_or(&input.token));
    if input.profile.is_some() {
        input.identity.push_str(&fingerprint(&input.token));
    }
    Ok(input)
}

#[derive(Default)]
pub struct Session {
    agy: agy_cloud::Session,
}

pub fn fetch(
    source: Source,
    input: &Prepared,
    http: &Http,
    config: &Config,
    session: &mut Session,
) -> Result<Vec<Card>, FetchError> {
    match source {
        Source::Codex => parse::codex(&http.get(
            "https://chatgpt.com/backend-api/wham/usage",
            &input.token,
            input.account.as_deref(),
        )?),
        Source::Go | Source::Go2 => parse::go(
            &http.get("https://opencode.ai/zen/go/v1/usage", &input.token, None)?,
            source.title(),
        ),
        Source::OpenRouter => parse::openrouter(&http.get(
            "https://openrouter.ai/api/v1/credits",
            &input.token,
            None,
        )?),
        Source::Grok => grok::parse(&http.grok(&input.token)?),
        Source::Agy | Source::Agy2 => {
            let profile = input.profile.as_deref().ok_or_else(FetchError::format)?;
            let cloud = session
                .agy
                .query(source, input.agy_credentials.as_ref(), http);
            let result = match cloud {
                Ok(cards) => Ok(cards),
                Err(error) if agy_cloud::allow_local_fallback(&error) => {
                    agy::fetch(source, profile, &config.home, http)
                        .map(|mut cards| {
                            for card in &mut cards {
                                card.note = Some("本地服务备用".into());
                            }
                            cards
                        })
                        .map_err(|_| error)
                }
                Err(error) => Err(error),
            };
            // A profile may be switched or logged out while a request is in flight.
            let current = agy_cloud::Credentials::load(profile)?;
            if current.as_ref().map(agy_cloud::Credentials::identity)
                != input
                    .agy_credentials
                    .as_ref()
                    .map(agy_cloud::Credentials::identity)
            {
                return Err(FetchError::auth("AGY 账户已变化 · 请刷新"));
            }
            result
        }
    }
}

fn opencode_key(root: &Path, provider: &str) -> Result<String, FetchError> {
    if let Ok(auth) = read_json(&root.join("auth.json"))
        && let Some(key) = auth
            .get(provider)
            .and_then(|v| v.get("key"))
            .and_then(Value::as_str)
    {
        return Ok(key.into());
    }
    let accounts = read_json(&root.join("account.json"))?;
    let active = accounts
        .get("active")
        .and_then(|v| v.get(provider))
        .and_then(Value::as_str)
        .ok_or_else(|| FetchError::auth("未配置余额查询凭据"))?;
    let account = accounts
        .get("accounts")
        .and_then(|v| v.get(active))
        .ok_or_else(FetchError::format)?;
    account
        .pointer("/credential/key")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| FetchError::auth("未配置余额查询凭据"))
}

pub fn read_json(path: &Path) -> Result<Value, FetchError> {
    serde_json::from_str(&read_text(path)?).map_err(|_| FetchError::auth("登录文件格式无法识别"))
}

fn read_text(path: &Path) -> Result<String, FetchError> {
    let mut text = String::new();
    File::open(path)
        .and_then(|file| file.take(1024 * 1024 + 1).read_to_string(&mut text))
        .map_err(|_| FetchError::auth("无法读取登录凭据"))?;
    if text.len() > 1024 * 1024 {
        return Err(FetchError::auth("登录文件过大"));
    }
    Ok(text)
}

pub fn fingerprint(value: &str) -> String {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}
