use serde::Deserialize;
use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileConfig {
    refresh_seconds: Option<u64>,
    codex_home: Option<String>,
    agy_home: Option<String>,
    agy2_home: Option<String>,
    opencode_home: Option<String>,
    grok_home: Option<String>,
    openrouter_key_file: Option<String>,
    usage_db: Option<String>,
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
            Ok(text) => {
                toml::from_str(&text).map_err(|_| format!("配置格式错误：{}", path.display()))?
            }
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
        let parsed: FileConfig = toml::from_str("usage_db = \"/tmp/other.db\"")
            .expect("usage_db must be a declared key");
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
usage_db = "/tmp/usage.db"
"#;
        let parsed: FileConfig = toml::from_str(text).expect("all keys declared");
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
        assert!(toml::from_str::<FileConfig>("not_a_key = 1").is_err());
    }
}
