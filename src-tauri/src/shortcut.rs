//! The system-wide quick-capture shortcut. It stays off until the user records one in Settings.
//! The choice belongs to this computer rather than to the plan data, so it lives in a small file
//! beside the database and never travels with exports or backups.

use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use tauri_plugin_global_shortcut::{Modifiers, Shortcut};

const FILE: &str = "shortcuts.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ShortcutPreferences {
    /// Opens quick capture from any app, in the canonical spelling such as "shift+super+Space";
    /// `None` while it's switched off.
    pub quick_capture: Option<String>,
}

pub fn preferences_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(FILE)
}

/// The saved preferences, or the defaults when the file is missing or unreadable.
pub fn load(path: &Path) -> ShortcutPreferences {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save(path: &Path, preferences: &ShortcutPreferences) -> AppResult<()> {
    fs::write(path, serde_json::to_vec_pretty(preferences)?)?;
    Ok(())
}

/// Reads a recorded key combination. A global shortcut takes its keys away from every other app
/// while Delve Planner runs, so it needs two modifiers and at least one of them must be Control,
/// Alt (Option), or Command (the Windows key): one modifier and a letter, like Command-C, belongs
/// to the app in front.
pub fn parse(text: &str) -> AppResult<Shortcut> {
    let shortcut = Shortcut::from_str(text.trim()).map_err(|_| {
        AppError::Validation("Delve Planner can't use that key combination as a shortcut.".into())
    })?;
    let modifiers = [
        Modifiers::SHIFT,
        Modifiers::CONTROL,
        Modifiers::ALT,
        Modifiers::SUPER | Modifiers::META,
    ]
    .into_iter()
    .filter(|modifier| shortcut.mods.intersects(*modifier))
    .count();
    let reaches_past_shift = shortcut
        .mods
        .intersects(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SUPER | Modifiers::META);
    if modifiers < 2 || !reaches_past_shift {
        return Err(AppError::Validation(
            "Use two modifier keys, such as Shift and Command or Control and Alt, so the shortcut doesn't take keys other apps use."
                .into(),
        ));
    }
    Ok(shortcut)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shortcut_needs_two_modifiers_beyond_shift_alone() {
        assert!(parse("shift+super+Space").is_ok());
        assert!(parse("Control+Alt+KeyN").is_ok());
        assert!(parse("CmdOrCtrl+Shift+KeyK").is_ok());
        for refused in ["super+KeyC", "shift+KeyA", "KeyQ", "Space", "shift+alt"] {
            assert!(parse(refused).is_err(), "{refused} should be refused");
        }
        assert!(parse("shift+super+NotAKey").is_err());
    }

    #[test]
    fn preferences_survive_a_round_trip_and_default_when_missing() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = preferences_path(directory.path());
        assert_eq!(load(&path), ShortcutPreferences::default());
        let chosen = ShortcutPreferences {
            quick_capture: Some("shift+super+Space".into()),
        };
        save(&path, &chosen).expect("saved");
        assert_eq!(load(&path), chosen);
        fs::write(&path, b"not json").expect("overwritten");
        assert_eq!(load(&path), ShortcutPreferences::default());
    }
}
