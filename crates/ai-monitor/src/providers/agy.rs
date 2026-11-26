//! Antigravity CLI owns the login and performs its built-in /usage query.
use super::{fingerprint, parse};
use crate::model::{Card, FetchError};
use std::{
    fs, io, os::unix::fs::MetadataExt, path::Path, process::Command, sync::atomic::AtomicBool,
    time::Duration,
};

pub(super) fn profile_identity(profile: &Path, title: &str) -> Result<String, FetchError> {
    // Check presence before spawning: an unauthenticated CLI can open a browser.
    // Metadata detects profile replacement without reading the OAuth document.
    let metadata = fs::metadata(profile.join("antigravity-cli/antigravity-oauth-token"))
        .ok()
        .filter(|m| m.is_file() && m.len() > 0)
        .ok_or_else(|| FetchError::auth(format!("请先登录 {title}")))?;
    Ok(fingerprint(&format!(
        "{:?}:{}:{}:{}:{}",
        profile,
        metadata.dev(),
        metadata.ino(),
        metadata.mtime(),
        metadata.mtime_nsec()
    )))
}

pub(super) fn fetch(
    profile: &Path,
    title: &str,
    home: &Path,
    stop: &AtomicBool,
) -> Result<Vec<Card>, FetchError> {
    // Local installations can wrap agy with interactive defaults.
    let native = home.join(".local/bin/agy-real");
    let mut command = Command::new(if native.is_file() {
        &native
    } else {
        Path::new("agy")
    });
    let profile = profile
        .canonicalize()
        .map_err(|_| FetchError::auth(format!("请先登录 {title}")))?;
    command
        .current_dir("/")
        .env("BROWSER", "/bin/false")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .arg("--gemini_dir")
        .arg(profile)
        .args([
            "-p",
            "/usage",
            "--output-format",
            "json",
            "--print-timeout",
            "15s",
        ]);
    let (status, output) = crate::subprocess::output(command, stop, Duration::from_secs(20))
        .map_err(|e| {
            FetchError::new(match e.kind() {
                io::ErrorKind::NotFound => "缺少 Antigravity CLI",
                io::ErrorKind::TimedOut => "AGY 额度查询超时",
                _ => "无法运行 AGY 额度查询",
            })
        })?;
    if !status.success() {
        return Err(FetchError::new(format!(
            "{title} /usage 失败，请检查 CLI 登录或网络"
        )));
    }
    parse::agy(
        &serde_json::from_str(&output).map_err(|_| FetchError::format())?,
        title,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn each_usage_query_is_bound_to_the_requested_cli_profile() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let bin = home.join(".local/bin/agy-real");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fs::write(&bin, r#"#!/bin/sh
[ "$1" = "--gemini_dir" ] && [ "$3" = "-p" ] && [ "$4" = "/usage" ] || exit 1
case "$2" in
  */.gemini) fraction=0.25 ;;
  */.gemini2) fraction=0.75 ;;
  *) exit 2 ;;
esac
printf '{"status":"SUCCESS","command":{"name":"usage","data":{"groups":[{"name":"Gemini Models","buckets":[{"window":"weekly","remaining_fraction":%s}]}]}}}' "$fraction"
"#).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        for (directory, title, expected) in [(".gemini", "AGY", 25.), (".gemini2", "AGY2", 75.)] {
            let profile = home.join(directory);
            assert!(profile_identity(&profile, title).unwrap_err().invalidate);
            let login = profile.join("antigravity-cli/antigravity-oauth-token");
            fs::create_dir_all(login.parent().unwrap()).unwrap();
            fs::write(login, "fixture").unwrap();
            profile_identity(&profile, title).unwrap();
            let cards = fetch(&profile, title, home, &AtomicBool::new(false)).unwrap();
            assert_eq!(cards[0].title, title);
            assert_eq!(cards[0].meters[0].remaining, Some(expected));
        }
    }
}
