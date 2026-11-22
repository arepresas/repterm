use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

const DEFAULT_BASE_URL: &str = "http://localhost:4000/v1";

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub include_terminal_context: bool,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("model", &self.model)
            .field("include_terminal_context", &self.include_terminal_context)
            .finish()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            api_key: String::new(),
            model: String::new(),
            include_terminal_context: true,
        }
    }
}

/// `$XDG_CONFIG_HOME/repterm/settings.json`, falling back to
/// `~/.config/repterm/settings.json`.
pub fn settings_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("repterm").join("settings.json")
}

/// Reads settings from `path`, falling back to defaults when the file is
/// missing or cannot be parsed.
pub fn load_from(path: &Path) -> Settings {
    match std::fs::File::open(path) {
        Ok(file) => serde_json::from_reader(file).unwrap_or_default(),
        Err(_) => Settings::default(),
    }
}

pub fn load() -> Settings {
    load_from(&settings_path())
}

impl Settings {
    /// Writes the settings as pretty JSON, creating the parent directory.
    /// On Unix the file is created with mode 0600 because it holds an API key.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        #[cfg(unix)]
        let file = {
            use std::os::unix::fs::OpenOptionsExt;
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(path)?
        };
        #[cfg(not(unix))]
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;

        serde_json::to_writer_pretty(file, self).map_err(std::io::Error::other)
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&settings_path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_point_to_local_litellm() {
        let s = Settings::default();
        assert_eq!(s.base_url, "http://localhost:4000/v1");
        assert!(s.model.is_empty());
        assert!(s.include_terminal_context);
    }

    #[test]
    fn missing_or_invalid_file_falls_back_to_defaults() {
        let dir =
            std::env::temp_dir().join(format!("repterm-settings-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.json");
        assert_eq!(load_from(&path).base_url, Settings::default().base_url);
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(load_from(&path).api_key, "");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_load_roundtrip_preserves_all_fields() {
        let dir = std::env::temp_dir().join(format!("repterm-roundtrip-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.json");
        let s = Settings {
            base_url: "http://host:4000/v1".into(),
            api_key: "secret".into(),
            model: "gpt-4o-mini".into(),
            include_terminal_context: false,
        };
        s.save_to(&path).unwrap();
        assert_eq!(load_from(&path).api_key, "secret");
        assert!(!load_from(&path).include_terminal_context);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn partial_file_keeps_present_fields_and_defaults_rest() {
        let dir = std::env::temp_dir().join(format!("repterm-partial-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.json");

        std::fs::write(&path, "{\"api_key\":\"sk-secret\"}").unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded.api_key, "sk-secret");
        assert_eq!(loaded.base_url, Settings::default().base_url);
        assert_eq!(loaded.model, Settings::default().model);
        assert_eq!(
            loaded.include_terminal_context,
            Settings::default().include_terminal_context
        );

        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(load_from(&path), Settings::default());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn debug_output_redacts_api_key() {
        let settings = Settings {
            api_key: "sk-secret".into(),
            ..Settings::default()
        };
        let text = format!("{settings:?}");
        assert!(text.contains("<redacted>"));
        assert!(!text.contains("sk-secret"));
    }
}
