//! API keys: environment variables first, then the OS keychain
//! (macOS Keychain, Windows Credential Manager, Secret Service on Linux).
//! Keys are never written to the config file, the session database or logs.

use common::proto::NoticeLevel;

use crate::Runtime;

pub const SERVICE: &str = "212";
pub const PROVIDERS: &[&str] = &["openai", "anthropic", "compat", "huggingface"];

fn entry(provider: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, provider).map_err(|e| format!("system keychain unavailable: {e}"))
}

/// The key for `provider`: `env_var` if set, else the keychain.
pub fn lookup(provider: &str, env_var: &str) -> Option<String> {
    if let Ok(v) = std::env::var(env_var)
        && !v.trim().is_empty()
    {
        return Some(v.trim().to_string());
    }
    if std::env::var_os("TWO_ONE_TWO_NO_KEYCHAIN").is_some() {
        return None;
    }
    entry(provider).ok()?.get_password().ok().filter(|k| !k.is_empty())
}

impl Runtime {
    pub(crate) async fn set_credential(&mut self, provider: &str, secret: String) {
        let provider = provider.trim().to_lowercase();
        if !PROVIDERS.contains(&provider.as_str()) {
            return self.notice(NoticeLevel::Error, format!("Unknown provider '{provider}'. Use one of: {}.", PROVIDERS.join(", "))).await;
        }
        let secret = secret.trim().to_string();
        if secret.is_empty() {
            return self.notice(NoticeLevel::Warn, "Empty key; nothing stored.").await;
        }
        let r = tokio::task::spawn_blocking(move || {
            entry(&provider).and_then(|e| e.set_password(&secret).map_err(|e| e.to_string())).map(|_| provider)
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        match r {
            Ok(p) => {
                self.notice(NoticeLevel::Success, format!("Saved the {p} key in the system keychain. Switch with /provider {p}.")).await
            }
            Err(e) => {
                self.notice(NoticeLevel::Error, format!("Could not store the key: {e}. You can use an environment variable instead.")).await
            }
        }
    }

    pub(crate) async fn remove_credential(&mut self, provider: &str) {
        let provider = provider.trim().to_lowercase();
        let p2 = provider.clone();
        let r = tokio::task::spawn_blocking(move || entry(&p2).and_then(|e| e.delete_credential().map_err(|e| e.to_string())))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        match r {
            Ok(()) => self.notice(NoticeLevel::Success, format!("Removed the {provider} key from the keychain.")).await,
            Err(e) => self.notice(NoticeLevel::Warn, format!("No {provider} key removed: {e}")).await,
        }
    }
}
