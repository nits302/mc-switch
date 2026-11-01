//! OpenCode provider.
//!
//! The whole credential lives in `~/.local/share/opencode/auth.json`, one
//! entry per service: `{"<service>": {"type": "oauth", "access", "refresh",
//! "expires", "accountId"?} | {"type": "api", "key"}}` (verified on a real
//! install, 2026-11). The vault blob is that file verbatim. A running
//! OpenCode may keep the login it read at start until restarted.
//!
//! The file names no person. Who it belongs to is read from its OAuth logins
//! (an OpenAI access token is a JWT naming its user and workspace); a file of
//! API keys alone is told apart by a short hash of each key, never the key.
//! OpenCode signs in through its own picker, so there is no private sign-in:
//! an account is added by naming the login OpenCode holds.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::codex::{email_from_jwt, jwt_claims};
use super::{ActiveCredential, Details, Provider, ProviderId};
use crate::error::Result;
use crate::host::Host;

pub struct OpenCode;

impl OpenCode {
    /// shortcut: ignores `XDG_DATA_HOME` (OpenCode honours it); read it here
    /// if someone moves their data dir.
    pub fn auth_path(host: &dyn Host) -> PathBuf {
        host.home()
            .join(".local")
            .join("share")
            .join("opencode")
            .join("auth.json")
    }
}

impl Provider for OpenCode {
    fn id(&self) -> ProviderId {
        ProviderId::OpenCode
    }

    fn read_active(&self, host: &dyn Host) -> Result<Option<ActiveCredential>> {
        let Some(blob) = host.read(&Self::auth_path(host))? else {
            return Ok(None);
        };
        Ok(Some(ActiveCredential {
            details: self.details(&blob),
            blob,
        }))
    }

    fn write_active(&self, host: &dyn Host, blob: &[u8]) -> Result<()> {
        host.write_atomic(&Self::auth_path(host), blob)
    }

    /// No private sign-in: see the module docs.
    fn read_signed_in(&self, _host: &dyn Host, _dir: &Path) -> Result<Option<ActiveCredential>> {
        Ok(None)
    }

    fn discard_signed_in(&self, _host: &dyn Host, _dir: &Path) -> Result<()> {
        Ok(())
    }

    /// Identity: each OAuth login as `<service>=<user>:<workspace>`; with no
    /// OAuth login, each API key as `<service>#<hash>`. The label is the
    /// first email an OAuth token names; the plan says which services it holds.
    fn details(&self, blob: &[u8]) -> Details {
        let Ok(auth) = serde_json::from_slice::<BTreeMap<String, Value>>(blob) else {
            return Details::default();
        };
        let mut oauth = Vec::new();
        let mut keys = Vec::new();
        let mut label = None;
        for (service, entry) in &auth {
            match entry.get("type").and_then(Value::as_str) {
                Some("oauth") => {
                    let access = entry.get("access").and_then(Value::as_str).unwrap_or("");
                    let claims = jwt_claims(access).unwrap_or(Value::Null);
                    let openai = &claims["https://api.openai.com/auth"];
                    let user = super::text(openai, "chatgpt_user_id")
                        .or_else(|| super::text(&claims, "sub"));
                    let workspace = super::text(entry, "accountId")
                        .or_else(|| super::text(openai, "chatgpt_account_id"));
                    if let Some(user) = user {
                        oauth.push(format!(
                            "{service}={user}:{}",
                            workspace.unwrap_or_default()
                        ));
                    }
                    if label.is_none() {
                        label = email_from_jwt(access);
                    }
                }
                _ => {
                    if let Some(key) = super::text(entry, "key") {
                        let hash = Sha256::digest(key.as_bytes());
                        let short: String =
                            hash.iter().take(6).map(|b| format!("{b:02x}")).collect();
                        keys.push(format!("{service}#{short}"));
                    }
                }
            }
        }
        let parts = if oauth.is_empty() { keys } else { oauth };
        let plan = match auth.len() {
            0 => None,
            1 => auth.keys().next().cloned(),
            n => Some(format!("{n} providers")),
        };
        Details {
            label,
            identity: (!parts.is_empty()).then(|| parts.join(";")),
            plan,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHost;
    use base64::Engine as _;

    fn fake_jwt(payload: &str) -> String {
        let b64 = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
        format!(
            "{}.{}.{}",
            b64(r#"{"alg":"RS256"}"#),
            b64(payload),
            b64("sig")
        )
    }

    fn auth(user: &str) -> String {
        let access = fake_jwt(&format!(
            r#"{{"https://api.openai.com/auth":{{"chatgpt_user_id":"{user}"}},"https://api.openai.com/profile":{{"email":"{user}@x.com"}}}}"#
        ));
        format!(
            r#"{{"zai":{{"type":"api","key":"secret-key"}},"openai":{{"type":"oauth","access":"{access}","refresh":"r","expires":1,"accountId":"ws"}}}}"#
        )
    }

    #[test]
    fn read_active_none_when_logged_out() {
        assert!(OpenCode.read_active(&FakeHost::new()).unwrap().is_none());
    }

    #[test]
    fn the_blob_is_the_file_verbatim_and_round_trips() {
        let host = FakeHost::new();
        let json = auth("u1");
        OpenCode.write_active(&host, json.as_bytes()).unwrap();
        let active = OpenCode.read_active(&host).unwrap().unwrap();
        assert_eq!(active.blob, json.as_bytes());
        assert_eq!(active.details.label.as_deref(), Some("u1@x.com"));
        assert_eq!(active.details.identity.as_deref(), Some("openai=u1:ws"));
        assert_eq!(active.details.plan.as_deref(), Some("2 providers"));
    }

    #[test]
    fn two_oauth_users_are_two_identities() {
        let a = OpenCode.details(auth("u1").as_bytes()).identity;
        let b = OpenCode.details(auth("u2").as_bytes()).identity;
        assert_ne!(a, b);
    }

    #[test]
    fn keys_alone_are_told_apart_by_hash_and_never_shown() {
        let one = OpenCode.details(br#"{"zai":{"type":"api","key":"k1"}}"#);
        let two = OpenCode.details(br#"{"zai":{"type":"api","key":"k2"}}"#);
        let id = one.identity.clone().unwrap();
        assert!(id.starts_with("zai#") && !id.contains("k1"));
        assert_ne!(one.identity, two.identity);
        assert_eq!(one.label, None);
        assert_eq!(one.plan.as_deref(), Some("zai"));
    }

    #[test]
    fn junk_says_nothing() {
        assert_eq!(OpenCode.details(b"not json"), Details::default());
    }
}
