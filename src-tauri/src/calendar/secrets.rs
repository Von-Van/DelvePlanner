//! Calendar links and account tokens are secrets: either one reads somebody's calendar. They
//! live in the macOS Keychain or Windows Credential Manager, one item per calendar or account,
//! and never in SQLite, exports, backups, logs, or the renderer. After the first read each one is
//! cached in memory, so a launch asks the keychain at most once for each.

use crate::error::{AppError, AppResult};
use std::collections::HashMap;
use std::sync::Mutex;

/// Where secrets are persisted.
pub trait SecretVault: Send + Sync {
    fn read(&self, account: &str) -> AppResult<Option<String>>;
    fn write(&self, account: &str, secret: &str) -> AppResult<()>;
    fn delete(&self, account: &str) -> AppResult<()>;
}

pub struct CalendarSecrets {
    vault: Box<dyn SecretVault>,
    cache: Mutex<HashMap<String, String>>,
}

impl CalendarSecrets {
    pub fn new(vault: Box<dyn SecretVault>) -> Self {
        Self {
            vault,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// A subscribed calendar's link.
    pub fn link(&self, calendar_id: &str) -> AppResult<Option<String>> {
        self.read(&link_account(calendar_id))
    }

    pub fn set_link(&self, calendar_id: &str, link: &str) -> AppResult<()> {
        self.write(&link_account(calendar_id), link)
    }

    /// Removes a link; one that was never stored is not an error.
    pub fn remove_link(&self, calendar_id: &str) -> AppResult<()> {
        self.forget(&link_account(calendar_id))
    }

    /// A connected account's refresh token, which stands in for its whole sign-in.
    pub fn refresh_token(&self, account_id: &str) -> AppResult<Option<String>> {
        self.read(&token_account(account_id))
    }

    pub fn set_refresh_token(&self, account_id: &str, token: &str) -> AppResult<()> {
        self.write(&token_account(account_id), token)
    }

    pub fn remove_refresh_token(&self, account_id: &str) -> AppResult<()> {
        self.forget(&token_account(account_id))
    }

    fn read(&self, account: &str) -> AppResult<Option<String>> {
        if let Some(secret) = self.cache()?.get(account) {
            return Ok(Some(secret.clone()));
        }
        let secret = self.vault.read(account)?;
        if let Some(secret) = &secret {
            self.cache()?.insert(account.into(), secret.clone());
        }
        Ok(secret)
    }

    fn write(&self, account: &str, secret: &str) -> AppResult<()> {
        self.vault.write(account, secret)?;
        self.cache()?.insert(account.into(), secret.into());
        Ok(())
    }

    fn forget(&self, account: &str) -> AppResult<()> {
        self.cache()?.remove(account);
        self.vault.delete(account)
    }

    fn cache(&self) -> AppResult<std::sync::MutexGuard<'_, HashMap<String, String>>> {
        self.cache
            .lock()
            .map_err(|_| AppError::Internal("Calendar secrets are unavailable.".into()))
    }
}

fn link_account(calendar_id: &str) -> String {
    format!("calendar-link:{calendar_id}")
}

fn token_account(account_id: &str) -> String {
    format!("calendar-account:{account_id}")
}

/// The system credential store for `service`, or a vault that refuses every operation when this
/// platform has none DayPlan supports.
pub fn system_vault(service: &str) -> Box<dyn SecretVault> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    match SystemKeychain::new(service) {
        Ok(keychain) => return Box::new(keychain),
        Err(_) => tauri_plugin_log::log::warn!("keychain_store_unavailable"),
    }
    let _ = service;
    Box::new(UnavailableVault)
}

struct UnavailableVault;

impl SecretVault for UnavailableVault {
    fn read(&self, _: &str) -> AppResult<Option<String>> {
        Err(AppError::Keychain)
    }

    fn write(&self, _: &str, _: &str) -> AppResult<()> {
        Err(AppError::Keychain)
    }

    fn delete(&self, _: &str) -> AppResult<()> {
        Err(AppError::Keychain)
    }
}

/// The macOS Keychain or Windows Credential Manager, under the app's bundle identifier.
#[cfg(any(target_os = "macos", target_os = "windows"))]
struct SystemKeychain {
    service: String,
    store: std::sync::Arc<keyring_core::CredentialStore>,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl SystemKeychain {
    fn new(service: &str) -> AppResult<Self> {
        #[cfg(target_os = "macos")]
        let store: std::sync::Arc<keyring_core::CredentialStore> =
            apple_native_keyring_store::keychain::Store::new().map_err(keychain_error)?;
        #[cfg(target_os = "windows")]
        let store: std::sync::Arc<keyring_core::CredentialStore> =
            windows_native_keyring_store::Store::new().map_err(keychain_error)?;
        Ok(Self {
            service: service.into(),
            store,
        })
    }

    fn entry(&self, account: &str) -> AppResult<keyring_core::Entry> {
        self.store
            .build(&self.service, account, None)
            .map_err(keychain_error)
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl SecretVault for SystemKeychain {
    fn read(&self, account: &str) -> AppResult<Option<String>> {
        match self.entry(account)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(error) => Err(keychain_error(error)),
        }
    }

    fn write(&self, account: &str, secret: &str) -> AppResult<()> {
        self.entry(account)?
            .set_password(secret)
            .map_err(keychain_error)
    }

    fn delete(&self, account: &str) -> AppResult<()> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(error) => Err(keychain_error(error)),
        }
    }
}

/// Logs only the kind of failure: platform details can name the item, never the secret, but the
/// logs stay free of both.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn keychain_error(error: keyring_core::Error) -> AppError {
    let kind = match error {
        keyring_core::Error::NoStorageAccess(_) => "no_access",
        keyring_core::Error::TooLong(..) => "too_long",
        keyring_core::Error::PlatformFailure(_) => "platform",
        _ => "other",
    };
    tauri_plugin_log::log::warn!("keychain_unavailable kind={kind}");
    AppError::Keychain
}

/// An in-memory vault for tests.
#[cfg(test)]
#[derive(Default)]
pub struct MemoryVault {
    pub secrets: Mutex<HashMap<String, String>>,
}

#[cfg(test)]
impl SecretVault for std::sync::Arc<MemoryVault> {
    fn read(&self, account: &str) -> AppResult<Option<String>> {
        Ok(self.secrets.lock().unwrap().get(account).cloned())
    }

    fn write(&self, account: &str, secret: &str) -> AppResult<()> {
        self.secrets
            .lock()
            .unwrap()
            .insert(account.into(), secret.into());
        Ok(())
    }

    fn delete(&self, account: &str) -> AppResult<()> {
        self.secrets.lock().unwrap().remove(account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn secrets_are_cached_after_the_first_read_and_removed_everywhere() {
        let vault = Arc::new(MemoryVault::default());
        let secrets = CalendarSecrets::new(Box::new(vault.clone()));
        assert_eq!(secrets.link("a").unwrap(), None);
        secrets
            .set_link("a", "https://calendar.example/private.ics")
            .unwrap();
        secrets.set_refresh_token("account-1", "rt-1").unwrap();
        let mut stored = vault
            .secrets
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        stored.sort();
        assert_eq!(stored, ["calendar-account:account-1", "calendar-link:a"]);
        vault.secrets.lock().unwrap().clear();
        assert_eq!(
            secrets.link("a").unwrap().as_deref(),
            Some("https://calendar.example/private.ics"),
            "later reads come from memory"
        );
        assert_eq!(
            secrets.refresh_token("account-1").unwrap().as_deref(),
            Some("rt-1")
        );
        secrets.remove_link("a").unwrap();
        secrets.remove_refresh_token("account-1").unwrap();
        secrets.remove_link("never-stored").unwrap();
        assert_eq!(secrets.link("a").unwrap(), None);
        assert_eq!(secrets.refresh_token("account-1").unwrap(), None);
    }
}
