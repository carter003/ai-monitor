use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};
/// 配置文件里出现的键。声明为常量而不是结构体字段：解析器是手写的平面
/// reader（见 `parse_config`），这个表既驱动解析也充当未知键守卫。
const KEYS: [&str; 9] = [
    "refresh_seconds",
    "codex_home",
    "agy_home",
    "agy2_home",
    "opencode_home",
    "grok_home",
    "openrouter_key_file",
    "go2_key_file",
    "usage_db",
];

#[derive(Default)]
struct FileConfig {
    refresh_seconds: Option<u64>,
    codex_home: Option<String>,
    agy_home: Option<String>,
    agy2_home: Option<String>,
    opencode_home: Option<String>,
    grok_home: Option<String>,
    openrouter_key_file: Option<String>,
    go2_key_file: Option<String>,
    usage_db: Option<String>,
}

/// 解析扁平的 `key = value` 配置。值是带引号字符串或无符号整数；注释
/// （`#`）与空行跳过；未知键或非法值报错，避免静默回退到默认配置。
fn parse_config(text: &str) -> Result<FileConfig, String> {
    let mut config = FileConfig::default();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("第 {} 行不是 `key = value`：{}", index + 1, raw));
        };
        let key = key.trim();
        if !KEYS.contains(&key) {
            return Err(format!("未知的配置键：{}", key));
        }
        let value = value.trim();
        let unquoted = value
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or(value);
        let is_quoted = unquoted.len() != value.len();
        match key {
            "refresh_seconds" => {
                config.refresh_seconds =
                    Some(unquoted.parse().map_err(|_| {
                        format!("refresh_seconds 必须是无符号整数，当前为 {}", value)
                    })?);
            }
            _ if is_quoted => match key {
                "openrouter_key_file" => config.openrouter_key_file = Some(unquoted.to_owned()),
                "go2_key_file" => config.go2_key_file = Some(unquoted.to_owned()),
                "codex_home" => config.codex_home = Some(unquoted.to_owned()),
                "agy_home" => config.agy_home = Some(unquoted.to_owned()),
                "agy2_home" => config.agy2_home = Some(unquoted.to_owned()),
                "opencode_home" => config.opencode_home = Some(unquoted.to_owned()),
                "grok_home" => config.grok_home = Some(unquoted.to_owned()),
                "usage_db" => config.usage_db = Some(unquoted.to_owned()),
                _ => unreachable!("KEYS 与 match 分支一一对应"),
            },
            _ => return Err(format!("{} 的值必须是带引号的字符串", key)),
        }
    }
    Ok(config)
}

#[derive(Clone)]
pub struct Config {
    pub refresh: Duration,
    pub home: PathBuf,
    pub codex: PathBuf,
    pub agy: PathBuf,
    pub agy2: PathBuf,
    pub opencode: PathBuf,
    pub grok: PathBuf,
    pub openrouter_key: PathBuf,
    /// 第二把 OpenCode Go 订阅 Key 的文件，一行纯文本。
    pub go2_key: PathBuf,
    /// sqlite database written by the herdr-usage collector. Read-only here.
    pub usage_db: PathBuf,
}

impl Config {
    pub fn load() -> Result<Self, String> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("无法确定用户目录")?;
        let config_root = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let path = env::var_os("AI_MONITOR_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|| config_root.join("ai-monitor/config.toml"));
        let file: FileConfig = match fs::read_to_string(&path) {
            Ok(text) => parse_config(&text)
                .map_err(|e| format!("配置格式错误：{}：{}", path.display(), e))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => FileConfig::default(),
            Err(_) => return Err(format!("无法读取配置：{}", path.display())),
        };
        let expand = |value: Option<String>, fallback: PathBuf| {
            value.map(|s| expand_home(&home, &s)).unwrap_or(fallback)
        };
        let seconds = file.refresh_seconds.unwrap_or(60);
        if !(15..=3600).contains(&seconds) {
            return Err("refresh_seconds 必须在 15 到 3600 之间".into());
        }
        Ok(Self {
            refresh: Duration::from_secs(seconds),
            codex: expand(
                file.codex_home,
                env::var_os("CODEX_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".codex")),
            ),
            agy: expand(file.agy_home, home.join(".gemini")),
            agy2: expand(file.agy2_home, home.join(".gemini2")),
            opencode: expand(
                file.opencode_home,
                env::var_os("XDG_DATA_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".local/share"))
                    .join("opencode"),
            ),
            grok: expand(
                file.grok_home,
                env::var_os("GROK_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".grok")),
            ),
            openrouter_key: expand(
                file.openrouter_key_file,
                config_root.join("ai-monitor/openrouter.key"),
            ),
            go2_key: expand(file.go2_key_file, config_root.join("ai-monitor/go2.key")),
            usage_db: expand(file.usage_db, home.join(".local/share/herdr/usage.db")),
            home,
        })
    }
}

pub fn expand_home(home: &Path, value: &str) -> PathBuf {
    if value == "~" {
        home.to_owned()
    } else if let Some(rest) = value.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `FileConfig` uses `deny_unknown_fields`, so every documented key must be
    /// declared. A key that is not would silently reset the whole config to
    /// defaults instead of raising, which is hard to notice in production.
    #[test]
    fn usage_db_is_an_accepted_config_key() {
        let parsed =
            parse_config("usage_db = \"/tmp/other.db\"").expect("usage_db must be a declared key");
        assert_eq!(parsed.usage_db.as_deref(), Some("/tmp/other.db"));
    }

    #[test]
    fn every_documented_key_parses_together() {
        let text = r#"
refresh_seconds = 120
codex_home = "/tmp/codex"
agy_home = "/tmp/agy"
agy2_home = "/tmp/agy2"
opencode_home = "/tmp/opencode"
grok_home = "/tmp/grok"
openrouter_key_file = "/tmp/key"
go2_key_file = "/tmp/go2.key"
usage_db = "/tmp/usage.db"
"#;
        let parsed = parse_config(text).expect("all keys declared");
        assert_eq!(parsed.refresh_seconds, Some(120));
        assert_eq!(parsed.usage_db.as_deref(), Some("/tmp/usage.db"));
    }

    #[test]
    fn a_tilde_is_expanded_for_the_usage_database() {
        let home = Path::new("/home/example");
        assert_eq!(
            expand_home(home, "~/.local/share/herdr/usage.db"),
            Path::new("/home/example/.local/share/herdr/usage.db")
        );
    }

    #[test]
    fn an_unknown_key_is_still_rejected() {
        // The guard that makes the two tests above necessary.
        assert!(parse_config("not_a_key = 1").is_err());
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let parsed = parse_config("# comment\n\nusage_db = \"x\" # trailing\n").unwrap();
        assert_eq!(parsed.usage_db.as_deref(), Some("x"));
    }

    #[test]
    fn unquoted_string_value_is_rejected() {
        assert!(parse_config("usage_db = x").is_err());
    }

    #[test]
    fn bad_integer_is_rejected() {
        assert!(parse_config("refresh_seconds = \"fast\"").is_err());
    }
}
