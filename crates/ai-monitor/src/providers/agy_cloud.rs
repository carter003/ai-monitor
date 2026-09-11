//! AGY subscription OAuth and quota state. Credentials never leave memory except
//! in requests to the fixed Google endpoints in Http; client files are read-only.
use super::{fingerprint, parse, read_json};
use crate::{
    http::Http,
    model::{Card, FetchError, Source},
};
use serde_json::Value;
use std::path::Path;

#[derive(Clone)] // No Debug: contains account credentials.
pub(super) struct Credentials {
    access: String,
    refresh: String,
    expiry: Option<i64>,
    display_name: Option<String>,
}

impl Credentials {
    pub(super) fn load(profile: &Path) -> Result<Option<Self>, FetchError> {
        let path = profile.join("antigravity-cli/antigravity-oauth-token");
        match path.try_exists() {
            Ok(false) => return Ok(None),
            Err(_) => return Err(FetchError::auth("无法读取 AGY 登录凭据")),
            Ok(true) => (),
        }
        Self::parse(&read_json(&path)?).map(Some)
    }

    fn parse(value: &Value) -> Result<Self, FetchError> {
        let token = value
            .get("token")
            .ok_or_else(|| FetchError::auth("AGY 登录格式无法识别"))?;
        let access = token
            .get("access_token")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let refresh = token
            .get("refresh_token")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if access.is_empty() && refresh.is_empty() {
            return Err(FetchError::auth("请重新登录 AGY 账户"));
        }
        Ok(Self {
            access,
            refresh,
            expiry: token.get("expiry").and_then(parse::timestamp),
            display_name: None,
        })
    }

    pub(super) fn identity(&self) -> String {
        // Access-token renewal is not an account switch. A different refresh
        // credential is treated conservatively as a new account/session.
        fingerprint(if self.refresh.is_empty() {
            &self.access
        } else {
            &self.refresh
        })
    }

    fn generation(&self) -> String {
        fingerprint(&format!(
            "{}:{}:{:?}",
            self.identity(),
            fingerprint(&self.access),
            self.expiry
        ))
    }

    fn needs_refresh(&self, now: i64) -> bool {
        self.access.is_empty()
            || self
                .expiry
                .is_some_and(|at| at <= now + if self.refresh.is_empty() { 0 } else { 90 })
    }

    fn renewed(&self, value: &Value, now: i64) -> Result<Self, FetchError> {
        let access = value
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(FetchError::format)?;
        let seconds = value
            .get("expires_in")
            .and_then(Value::as_i64)
            .filter(|s| *s > 90 && *s <= 86400)
            .ok_or_else(FetchError::format)?;
        if value
            .get("token_type")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.eq_ignore_ascii_case("bearer"))
        {
            return Err(FetchError::format());
        }
        Ok(Self {
            access: access.into(),
            refresh: value
                .get("refresh_token")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or(&self.refresh)
                .into(),
            expiry: Some(now + seconds),
            display_name: self.display_name.clone(),
        })
    }
}

trait Transport {
    fn quota(&self, token: &str) -> Result<Value, FetchError>;
    fn refresh(&self, token: &str) -> Result<Value, FetchError>;
    fn userinfo(&self, token: &str) -> Result<Value, FetchError>;
}

impl Transport for Http {
    fn userinfo(&self, token: &str) -> Result<Value, FetchError> {
        self.get(
            "https://openidconnect.googleapis.com/v1/userinfo",
            token,
            None,
        )
    }
    fn quota(&self, token: &str) -> Result<Value, FetchError> {
        self.agy_quota(token)
    }
    fn refresh(&self, token: &str) -> Result<Value, FetchError> {
        self.agy_refresh(token)
    }
}

#[derive(Default)]
pub(super) struct Session {
    cached: Option<(String, Credentials)>,
}

pub(super) fn allow_local_fallback(error: &FetchError) -> bool {
    !error.invalidate && error.retry_after.is_none()
}

impl Session {
    pub(super) fn query(
        &mut self,
        source: Source,
        credentials: Option<&Credentials>,
        http: &Http,
    ) -> Result<Vec<Card>, FetchError> {
        self.query_at(source, credentials, http, chrono::Utc::now().timestamp())
    }

    fn query_at(
        &mut self,
        source: Source,
        credentials: Option<&Credentials>,
        http: &impl Transport,
        now: i64,
    ) -> Result<Vec<Card>, FetchError> {
        let Some(credentials) = credentials else {
            self.cached = None;
            return Err(FetchError::new(format!(
                "未找到登录凭据 · 请登录 {}",
                if source == Source::Agy2 {
                    "agy2"
                } else {
                    "agy"
                }
            )));
        };
        let generation = credentials.generation();
        if self
            .cached
            .as_ref()
            .is_none_or(|(key, _)| key != &generation)
        {
            self.cached = Some((generation, credentials.clone()));
        }
        let token = &mut self.cached.as_mut().expect("initialized above").1;
        let refresh_first = token.needs_refresh(now);
        if refresh_first {
            Self::renew(token, http, now)?;
        }
        let summary = match http.quota(&token.access) {
            Err(error) if error.invalidate && !refresh_first => {
                // A server can reject a token before its stated expiry. Retry once.
                token.expiry = Some(now);
                Self::renew(token, http, now)?;
                http.quota(&token.access)?
            }
            result => result?,
        };
        let mut cards = parse::agy(&summary, source.title())?;
        if token.display_name.is_none() {
            token.display_name = http.userinfo(&token.access).ok().and_then(|value| {
                ["name", "email"].iter().find_map(|key| {
                    value
                        .get(key)
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned)
                })
            });
        }
        if let Some(name) = &token.display_name {
            for card in &mut cards {
                card.title = super::agy::account_title(source, name);
            }
        }
        Ok(cards)
    }

    fn renew(token: &mut Credentials, http: &impl Transport, now: i64) -> Result<(), FetchError> {
        if token.refresh.is_empty() {
            return Err(FetchError::auth("AGY 登录已过期 · 请重新登录"));
        }
        *token = token.renewed(&http.refresh(&token.refresh)?, now)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{cell::RefCell, collections::VecDeque};

    #[derive(Default)]
    struct Fake {
        calls: RefCell<Vec<String>>,
        quotas: RefCell<VecDeque<Result<Value, FetchError>>>,
        renewals: RefCell<VecDeque<Result<Value, FetchError>>>,
    }
    impl Transport for Fake {
        fn userinfo(&self, token: &str) -> Result<Value, FetchError> {
            Ok(json!({"name": format!("Google {token}")}))
        }
        fn quota(&self, token: &str) -> Result<Value, FetchError> {
            self.calls.borrow_mut().push(format!("quota:{token}"));
            self.quotas
                .borrow_mut()
                .pop_front()
                .expect("unexpected quota request")
        }
        fn refresh(&self, token: &str) -> Result<Value, FetchError> {
            self.calls.borrow_mut().push(format!("refresh:{token}"));
            self.renewals
                .borrow_mut()
                .pop_front()
                .expect("unexpected renewal")
        }
    }
    fn creds(access: &str, refresh: &str, expiry: i64) -> Credentials {
        Credentials {
            access: access.into(),
            refresh: refresh.into(),
            expiry: Some(expiry),
            display_name: None,
        }
    }
    fn summary(fraction: f64) -> Value {
        json!({"groups":[{"displayName":"Gemini Models","buckets":[
            {"bucketId":"gemini-5h","remainingFraction":1.0},
            {"bucketId":"gemini-weekly","remainingFraction":fraction}]}]})
    }
    #[test]
    fn account_names_follow_account_switches_and_survive_refresh() {
        let http = Fake::default();
        http.quotas
            .borrow_mut()
            .extend((0..3).map(|_| Ok(summary(0.5))));
        http.renewals.borrow_mut().push_back(Ok(renewal("renewed")));
        let mut session = Session::default();
        let first = creds("alice", "refresh-a", 200);
        assert_eq!(
            session
                .query_at(Source::Agy, Some(&first), &http, 0)
                .unwrap()[0]
                .title,
            "AGY（Google alice）"
        );
        assert_eq!(
            session
                .query_at(Source::Agy, Some(&first), &http, 150)
                .unwrap()[0]
                .title,
            "AGY（Google alice）"
        );
        let second = creds("bob", "refresh-b", 1000);
        assert_eq!(
            session
                .query_at(Source::Agy2, Some(&second), &http, 150)
                .unwrap()[0]
                .title,
            "AGY2（Google bob）"
        );
    }
    fn renewal(token: &str) -> Value {
        json!({"access_token":token,"expires_in":3599})
    }

    #[test]
    fn expired_file_renews_once_then_reuses_memory_token() {
        let fake = Fake::default();
        fake.renewals.borrow_mut().push_back(Ok(renewal("new")));
        fake.quotas
            .borrow_mut()
            .extend([Ok(summary(0.48)), Ok(summary(0.47))]);
        let mut session = Session::default();
        let old = creds("old", "refresh-a", 1);
        session
            .query_at(Source::Agy, Some(&old), &fake, 100)
            .unwrap();
        let result = session
            .query_at(Source::Agy, Some(&old), &fake, 160)
            .unwrap();
        assert_eq!(result[0].meters[1].remaining, Some(47.0));
        assert_eq!(
            *fake.calls.borrow(),
            ["refresh:refresh-a", "quota:new", "quota:new"]
        );
    }

    #[test]
    fn unauthorized_access_renews_and_retries_once() {
        let fake = Fake::default();
        fake.quotas
            .borrow_mut()
            .extend([Err(FetchError::auth("401")), Ok(summary(0.48))]);
        fake.renewals.borrow_mut().push_back(Ok(renewal("new")));
        Session::default()
            .query_at(Source::Agy, Some(&creds("old", "r", 5000)), &fake, 100)
            .unwrap();
        assert_eq!(
            *fake.calls.borrow(),
            ["quota:old", "refresh:r", "quota:new"]
        );
    }

    #[test]
    fn rotated_refresh_and_second_lifetime_are_preserved_in_memory() {
        let fake = Fake::default();
        fake.renewals.borrow_mut().extend([
            Ok(json!({"access_token":"first","refresh_token":"rotated","expires_in":3599})),
            Ok(renewal("second")),
        ]);
        fake.quotas
            .borrow_mut()
            .extend([Ok(summary(0.48)), Ok(summary(0.47))]);
        let original = creds("expired", "original", 1);
        let mut session = Session::default();
        session
            .query_at(Source::Agy, Some(&original), &fake, 100)
            .unwrap();
        session
            .query_at(Source::Agy, Some(&original), &fake, 3700)
            .unwrap();
        assert_eq!(
            *fake.calls.borrow(),
            [
                "refresh:original",
                "quota:first",
                "refresh:rotated",
                "quota:second"
            ]
        );
    }

    #[test]
    fn rejected_renewed_token_does_not_loop() {
        let fake = Fake::default();
        fake.quotas
            .borrow_mut()
            .extend([Err(FetchError::auth("401")), Err(FetchError::auth("401"))]);
        fake.renewals.borrow_mut().push_back(Ok(renewal("new")));
        assert!(
            Session::default()
                .query_at(Source::Agy, Some(&creds("old", "r", 5000)), &fake, 100)
                .unwrap_err()
                .invalidate
        );
        assert_eq!(fake.calls.borrow().len(), 3);
    }

    #[test]
    fn changed_file_access_token_replaces_cached_session() {
        let fake = Fake::default();
        fake.renewals.borrow_mut().push_back(Ok(renewal("memory")));
        fake.quotas
            .borrow_mut()
            .extend([Ok(summary(0.48)), Ok(summary(0.47))]);
        let mut session = Session::default();
        session
            .query_at(Source::Agy, Some(&creds("old", "r", 1)), &fake, 100)
            .unwrap();
        session
            .query_at(
                Source::Agy,
                Some(&creds("native-new", "r", 5000)),
                &fake,
                160,
            )
            .unwrap();
        assert_eq!(
            *fake.calls.borrow(),
            ["refresh:r", "quota:memory", "quota:native-new"]
        );
    }

    #[test]
    fn profile_files_are_independent_and_read_only() {
        let root = tempfile::tempdir().unwrap();
        for (profile, refresh) in [("one", "ra"), ("two", "rb")] {
            let dir = root.path().join(profile).join("antigravity-cli");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("antigravity-oauth-token"),
                json!({"token": {"access_token": "a", "refresh_token": refresh}}).to_string(),
            )
            .unwrap();
        }
        let a = Credentials::load(&root.path().join("one"))
            .unwrap()
            .unwrap();
        let b = Credentials::load(&root.path().join("two"))
            .unwrap()
            .unwrap();
        assert_ne!(a.identity(), b.identity());
        assert!(
            Credentials::load(&root.path().join("missing"))
                .unwrap()
                .is_none()
        );
        std::fs::write(
            root.path()
                .join("one/antigravity-cli/antigravity-oauth-token"),
            "broken",
        )
        .unwrap();
        assert!(
            Credentials::load(&root.path().join("one"))
                .err()
                .unwrap()
                .invalidate
        );
    }

    #[test]
    #[ignore = "explicit live Google credential and quota verification"]
    fn live_two_profiles_renew_without_client_or_file_writes() {
        let config = crate::config::Config::load().unwrap();
        let http = Http::new().unwrap();
        for (source, root) in [(Source::Agy, &config.agy), (Source::Agy2, &config.agy2)] {
            let path = root.join("antigravity-cli/antigravity-oauth-token");
            let before = std::fs::read(&path).unwrap();
            let mut credentials = Credentials::load(root).unwrap().unwrap();
            // Force the real refresh branch without changing the client's file.
            credentials.expiry = Some(0);
            let mut session = Session::default();
            for _ in 0..2 {
                let cards = session.query(source, Some(&credentials), &http).unwrap();
                assert!(cards[0].title.contains('（'), "Google account name missing");
                assert!(
                    cards[0]
                        .meters
                        .iter()
                        .any(|m| m.label == "5H" && m.remaining.is_some())
                );
                assert!(
                    cards[0]
                        .meters
                        .iter()
                        .any(|m| m.label == "周" && m.remaining.is_some())
                );
                println!("{}: {:?}", source.title(), cards[0].meters);
            }
            assert!(
                before == std::fs::read(path).unwrap(),
                "client credential file changed"
            );
        }
    }

    #[test]
    fn revoked_refresh_is_not_hidden_by_local_fallback() {
        let fake = Fake::default();
        fake.renewals
            .borrow_mut()
            .push_back(Err(FetchError::auth("重新登录")));
        let error = Session::default()
            .query_at(Source::Agy, Some(&creds("old", "r", 1)), &fake, 100)
            .unwrap_err();
        assert!(error.invalidate);
        assert!(!allow_local_fallback(&error));
        assert_eq!(fake.calls.borrow().len(), 1);
    }

    #[test]
    fn changed_login_and_two_sessions_do_not_share_tokens() {
        let fake = Fake::default();
        fake.renewals.borrow_mut().push_back(Ok(renewal("a-new")));
        fake.quotas
            .borrow_mut()
            .extend([Ok(summary(0.48)), Ok(summary(0.67)), Ok(summary(0.8))]);
        let mut first = Session::default();
        first
            .query_at(Source::Agy, Some(&creds("a", "ra", 1)), &fake, 100)
            .unwrap();
        Session::default()
            .query_at(Source::Agy2, Some(&creds("b", "rb", 5000)), &fake, 100)
            .unwrap();
        first
            .query_at(Source::Agy, Some(&creds("c", "rc", 5000)), &fake, 100)
            .unwrap();
        assert_eq!(
            *fake.calls.borrow(),
            ["refresh:ra", "quota:a-new", "quota:b", "quota:c"]
        );
    }

    #[test]
    fn logout_clears_cache_and_allows_local_only_when_no_file() {
        let fake = Fake::default();
        let mut session = Session {
            cached: Some(("old".into(), creds("a", "r", 5000))),
        };
        let error = session
            .query_at(Source::Agy2, None, &fake, 100)
            .unwrap_err();
        assert!(session.cached.is_none());
        assert!(error.message.contains("agy2"));
        assert!(allow_local_fallback(&error));
        assert!(fake.calls.borrow().is_empty());
    }

    #[test]
    fn identity_survives_access_renewal_but_not_account_change() {
        assert_eq!(
            creds("a", "r", 1).identity(),
            creds("b", "r", 5000).identity()
        );
        assert_ne!(
            creds("a", "r", 1).identity(),
            creds("b", "other", 5000).identity()
        );
        assert!(Credentials::parse(&json!({"token":{}})).is_err());
        assert!(
            creds("a", "r", 1)
                .renewed(&json!({"access_token":"new","expires_in":-1}), 100)
                .is_err()
        );
    }

    #[test]
    fn rate_limit_does_not_trigger_renewal_or_local_fallback() {
        let fake = Fake::default();
        let mut error = FetchError::new("429");
        error.retry_after = Some(std::time::Duration::from_secs(60));
        fake.quotas.borrow_mut().push_back(Err(error));
        let result = Session::default()
            .query_at(Source::Agy, Some(&creds("a", "r", 5000)), &fake, 100)
            .unwrap_err();
        assert!(!allow_local_fallback(&result));
        assert_eq!(*fake.calls.borrow(), ["quota:a"]);
    }
}
