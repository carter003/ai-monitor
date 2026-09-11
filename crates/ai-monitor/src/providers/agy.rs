use super::parse;
use crate::{
    http::Http,
    model::{Card, FetchError, Source},
};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

struct Candidate {
    pid: u32,
    start_time: String,
    csrf: String,
    ports: Vec<u16>,
}

pub fn fetch(
    source: Source,
    profile: &Path,
    home: &Path,
    http: &Http,
) -> Result<Vec<Card>, FetchError> {
    let candidates = discover(Path::new("/proc"), profile, home);
    if candidates.is_empty() {
        return Err(FetchError::new(if source == Source::Agy2 {
            "请启动 agy2 后刷新"
        } else {
            "请启动 agy 后刷新"
        }));
    }
    let deadline = Instant::now() + Duration::from_secs(12);
    for candidate in candidates {
        for port in candidate.ports {
            for tls in [false, true] {
                if Instant::now() > deadline {
                    return Err(FetchError::new("AGY 额度查询超时"));
                }
                // Recheck process identity before trusting a discovered socket.
                if process_start(&PathBuf::from(format!("/proc/{}", candidate.pid))).as_deref()
                    != Some(&candidate.start_time)
                {
                    break;
                }
                let Ok(value) = http.local_json(
                    port,
                    tls,
                    &candidate.csrf,
                    "RetrieveUserQuotaSummary",
                    &json!({"forceRefresh":true}),
                ) else {
                    continue;
                };
                let Ok(mut cards) = parse::agy(&value, source.title()) else {
                    continue;
                };
                let identity = http.local_json(
                    port,
                    tls,
                    &candidate.csrf,
                    "GetUserStatus",
                    &json!({"metadata":{"ideName":"antigravity","locale":"en"}}),
                );
                // The socket must belong to this profile's process and the service
                // must identify a signed-in account, not an anonymous cached server.
                let signed_in = identity
                    .as_ref()
                    .ok()
                    .is_some_and(|v| account_email(v).is_some());
                if signed_in {
                    if let Some(email) = identity.as_ref().ok().and_then(account_email) {
                        for card in &mut cards {
                            card.title = account_title(source, email);
                        }
                    }
                    return Ok(cards);
                }
            }
        }
    }
    Err(FetchError::new("AGY 尚未返回可用额度"))
}

pub(super) fn account_title(source: Source, name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    format!("{}（{name}）", source.title())
}

fn account_email(value: &Value) -> Option<&str> {
    [
        "/userStatus/email",
        "/userStatus/user/email",
        "/user/email",
        "/email",
        "/response/userStatus/email",
    ]
    .iter()
    .find_map(|path| {
        value
            .pointer(path)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    })
}

fn discover(proc_root: &Path, profile: &Path, home: &Path) -> Vec<Candidate> {
    let Ok(entries) = fs::read_dir(proc_root) else {
        return vec![];
    };
    let mut candidates = vec![];
    // SAFETY: geteuid has no preconditions or side effects.
    let uid = unsafe { libc::geteuid() };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let root = entry.path();
        if !root.metadata().is_ok_and(|m| m.uid() == uid) {
            continue;
        }
        let Ok(args) = fs::read(root.join("cmdline")) else {
            continue;
        };
        let args: Vec<String> = args
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        let Some(program) = args
            .first()
            .and_then(|s| Path::new(s).file_name())
            .and_then(|s| s.to_str())
        else {
            continue;
        };
        if !(program == "agy" || program == "agy2" || program.starts_with("agy.")) {
            continue;
        }
        if profile_from_args(&args, home) != profile {
            continue;
        }
        let Some(start_time) = process_start(&root) else {
            continue;
        };
        let ports = process_ports(&root);
        if ports.is_empty() {
            continue;
        }
        let csrf = flag(&args, "--csrf_token").unwrap_or_default();
        candidates.push(Candidate {
            pid,
            start_time,
            ports,
            csrf,
        });
    }
    candidates.sort_by_key(|c| std::cmp::Reverse(c.pid));
    candidates.truncate(8);
    candidates
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().enumerate().find_map(|(i, arg)| {
        if arg == name {
            args.get(i + 1).cloned()
        } else {
            arg.strip_prefix(&format!("{name}=")).map(str::to_owned)
        }
    })
}

fn profile_from_args(args: &[String], home: &Path) -> PathBuf {
    flag(args, "--gemini_dir")
        .map(|s| crate::config::expand_home(home, &s))
        .unwrap_or_else(|| home.join(".gemini"))
}

fn process_start(root: &Path) -> Option<String> {
    let stat = fs::read_to_string(root.join("stat")).ok()?;
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)
        .map(str::to_owned)
}

fn process_ports(root: &Path) -> Vec<u16> {
    let Ok(fds) = fs::read_dir(root.join("fd")) else {
        return vec![];
    };
    let inodes: HashSet<String> = fds
        .flatten()
        .filter_map(|entry| {
            let link = fs::read_link(entry.path()).ok()?;
            let link = link.to_str()?;
            link.strip_prefix("socket:[")?
                .strip_suffix(']')
                .map(str::to_owned)
        })
        .collect();
    let mut ports = HashSet::new();
    for file in ["net/tcp", "net/tcp6"] {
        if let Ok(text) = fs::read_to_string(root.join(file)) {
            ports.extend(listening_ports(&text, &inodes));
        }
    }
    let mut ports: Vec<u16> = ports.into_iter().collect();
    ports.sort_unstable();
    ports.truncate(6);
    ports
}

fn listening_ports(text: &str, inodes: &HashSet<String>) -> Vec<u16> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() <= 9 || fields[3] != "0A" || !inodes.contains(fields[9]) {
                return None;
            }
            let (address, port) = fields[1].rsplit_once(':')?;
            // Only a local/wildcard listener can be probed through loopback.
            if !matches!(
                address,
                "0100007F"
                    | "00000000"
                    | "00000000000000000000000001000000"
                    | "00000000000000000000000000000000"
            ) {
                return None;
            }
            u16::from_str_radix(port, 16).ok().filter(|p| *p != 0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accounts_are_matched_by_profile_not_process_name() {
        let home = Path::new("/home/test");
        let args = vec!["agy".into(), "--gemini_dir=/home/test/.gemini2".into()];
        assert_eq!(profile_from_args(&args, home), home.join(".gemini2"));
        assert_ne!(profile_from_args(&args, home), home.join(".gemini"));
        assert_eq!(
            profile_from_args(&["agy".into()], home),
            home.join(".gemini")
        );
    }
    #[test]
    fn only_own_listening_sockets_are_considered() {
        let text = "0: 0100007F:1F90 00000000:0000 0A 0 0 0 1000 0 111\n1: 0100007F:1F91 00000000:0000 0A 0 0 0 1000 0 222\n2: 0100007F:1F92 00000000:0000 01 0 0 0 1000 0 111\n";
        let inodes = HashSet::from(["111".to_string()]);
        assert_eq!(listening_ports(text, &inodes), vec![8080]);
    }
}
