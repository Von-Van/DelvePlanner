//! A calendar link is a secret: anyone who has it can read the calendar. Links live in the macOS
//! Keychain or Windows Credential Manager, one item per calendar, and never in SQLite, exports,
//! backups, logs, or the renderer. After the first read each link is cached in memory, so a
//! launch asks the keychain at most once per calendar.

use crate::error::{AppError, AppResult};
use std::collections::HashMap;
use std::sync::Mutex;

/// Where secrets are persisted.
pub trait SecretVault: Send + Sync {
    fn read(&self, account: &str) -> AppResult<Option<String>>;
    fn write(&self, account: &str, secret: &str) -> AppResult<()>;
    fn delete(&self, account: &str) -> AppResult<()>;
}

pub struct CalendarLinks {
    vault: Box<dyn SecretVault>,
    cache: Mutex<HashMap<String, String>>,
}

impl CalendarLinks {
    pub fn new(vault: Box<dyn SecretVault>) -> Self {
        Self {
            vault,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn get(&self, calendar_id: &str) -> AppResult<Option<String>> {
        if let Some(link) = self.cache()?.get(calendar_id) {
            return Ok(Some(link.clone()));
        }
        let link = self.vault.read(&account(calendar_id))?;
        if let Some(link) = &link {
            self.cache()?.insert(calendar_id.into(), link.clone());
        }
        Ok(link)
    }

    pub fn set(&self, calendar_id: &str, link: &str) -> AppResult<()> {
        self.vault.write(&account(calendar_id), link)?;
        self.cache()?.insert(calendar_id.into(), link.into());
        Ok(())
    }

    /// Removes the link; a link that was never stored is not an error.
    pub fn remove(&self, calendar_id: &str) -> AppResult<()> {
        self.cache()?.remove(calendar_id);
        self.vault.delete(&account(calendar_id))
    }

    fn cache(&self) -> AppResult<std::sync::MutexGuard<'_, HashMap<String, String>>> {
        self.cache
            .lock()
            .map_err(|_| AppError::Internal("Calendar links are unavailable.".into()))
    }
}

fn account(calendar_id: &str) -> String {
    format!("calendar-link:{calendar_id}")
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
    fn links_are_cached_after_the_first_read_and_removed_everywhere() {
        let vault = Arc::new(MemoryVault::default());
        let links = CalendarLinks::new(Box::new(vault.clone()));
        assert_eq!(links.get("a").unwrap(), None);
        links
            .set("a", "https://calendar.example/private.ics")
            .unwrap();
        assert_eq!(
            vault.secrets.lock().unwrap().keys().collect::<Vec<_>>(),
            ["calendar-link:a"]
        );
        vault.secrets.lock().unwrap().clear();
        assert_eq!(
            links.get("a").unwrap().as_deref(),
            Some("https://calendar.example/private.ics"),
            "later reads come from memory"
        );
        links.remove("a").unwrap();
        links.remove("never-stored").unwrap();
        assert_eq!(links.get("a").unwrap(), None);
    }
}
