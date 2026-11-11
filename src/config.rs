//! Persistent settings and saved SSH sessions (`~/.config/terminal-omega/config.json`).

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SshSession {
    pub name: String,
    /// Hostname, IP, or an alias from `~/.ssh/config`.
    pub host: String,
    /// Empty means "let ssh decide".
    pub user: String,
    pub port: u16,
    /// Empty means "let ssh decide".
    pub identity_file: String,
    /// Names this session's password in the system keychain.
    pub id: String,
    /// True when a password is saved in the keychain. The password itself
    /// is never written to this file.
    pub password_saved: bool,
}

impl Default for SshSession {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            user: String::new(),
            port: 22,
            identity_file: String::new(),
            id: String::new(),
            password_saved: false,
        }
    }
}

/// A new unique session id.
pub fn new_session_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let count = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}-{:x}-{count:x}", std::process::id())
}

impl SshSession {
    /// Returns a description of the problem if the session can't be used.
    pub fn validate(&self) -> Result<(), String> {
        let host = self.host.trim();
        if host.is_empty() {
            return Err("Host is required".into());
        }
        if host.starts_with('-') || host.chars().any(char::is_whitespace) {
            return Err("Host can't start with '-' or contain spaces".into());
        }
        if self.user.starts_with('-') || self.user.chars().any(char::is_whitespace) {
            return Err("User can't start with '-' or contain spaces".into());
        }
        if self.port == 0 {
            return Err("Port must be between 1 and 65535".into());
        }
        Ok(())
    }

    pub fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            self.host.clone()
        } else {
            self.name.clone()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub sessions: Vec<SshSession>,
    pub theme: String,
    pub font_size: f32,
    /// Optional path to a monospace TTF/OTF that replaces the default font.
    pub font_path: String,
    pub foreground_override: Option<[u8; 3]>,
    pub background_override: Option<[u8; 3]>,
    pub background_image: String,
    /// How visible the background image is, 0.0 to 1.0.
    pub background_image_strength: f32,
    /// Empty means the user's login shell.
    pub local_shell: String,
    pub scrollback: usize,
    /// Fraction of the window width used by the left pane.
    pub split_x: f32,
    /// Fraction of the right column height used by the btop pane.
    pub split_y: f32,
    /// Look for new releases and download them in the background.
    pub auto_update: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sessions: Vec::new(),
            theme: "Omega".into(),
            font_size: 14.0,
            font_path: String::new(),
            foreground_override: None,
            background_override: None,
            background_image: String::new(),
            background_image_strength: 0.25,
            local_shell: String::new(),
            scrollback: 10_000,
            split_x: 0.5,
            split_y: 0.5,
            auto_update: true,
        }
    }
}

fn config_path() -> Option<PathBuf> {
    Some(
        dirs::config_dir()?
            .join("terminal-omega")
            .join("config.json"),
    )
}

impl Config {
    /// Loads the config. An unreadable file is moved aside instead of being
    /// overwritten, and the returned warning tells the user where it went.
    pub fn load() -> (Self, Option<String>) {
        let Some(path) = config_path() else {
            return (Self::default(), None);
        };
        let Ok(text) = fs::read_to_string(&path) else {
            return (Self::default(), None);
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(mut config) => {
                // Sessions saved by 1.0 have no id yet.
                for session in &mut config.sessions {
                    if session.id.is_empty() {
                        session.id = new_session_id();
                    }
                }
                (config, None)
            }
            Err(err) => {
                let backup = path.with_extension("json.bad");
                let _ = fs::rename(&path, &backup);
                let warning = format!(
                    "Config could not be read ({err}). It was moved to {}.",
                    backup.display()
                );
                (Self::default(), Some(warning))
            }
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = config_path().ok_or("no config directory on this system")?;
        let dir = path.parent().ok_or("invalid config path")?;
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;

        // Write then rename, so a crash can't leave a half-written config.
        let tmp = path.with_extension("json.tmp");
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        file.write_all(json.as_bytes()).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_option_like_hosts() {
        let session = SshSession {
            host: "-oProxyCommand=x".into(),
            ..Default::default()
        };
        assert!(session.validate().is_err());
    }

    #[test]
    fn accepts_ssh_config_alias() {
        let session = SshSession {
            host: "prod-box".into(),
            ..Default::default()
        };
        assert!(session.validate().is_ok());
    }

    #[test]
    fn session_ids_are_unique() {
        assert_ne!(new_session_id(), new_session_id());
    }

    #[test]
    fn missing_fields_use_defaults() {
        let config: Config = serde_json::from_str(r#"{"font_size": 18.0}"#).unwrap();
        assert_eq!(config.font_size, 18.0);
        assert_eq!(config.split_x, 0.5);
    }
}
