//! Builds `ssh` command lines.
//!
//! The system `ssh` is used so keys, agents, `~/.ssh/config` and `known_hosts`
//! keep working. Each main tab owns a multiplexing socket: the user logs in
//! once in the main terminal and the btop pane rides on that connection, so
//! it never has to authenticate.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::SshSession;

/// Run through `sh` on the target. Exits 127 when btop isn't installed.
/// A non-interactive PATH often omits these directories, so they are added.
/// btop refuses to start without a UTF-8 locale, which SSH sessions on minimal
/// servers often lack, so one is set if needed.
pub const BTOP_SCRIPT: &str = "PATH=\"$PATH:/usr/local/bin:/snap/bin:$HOME/.local/bin:\
/home/linuxbrew/.linuxbrew/bin:/opt/homebrew/bin\"; export PATH; \
command -v btop >/dev/null 2>&1 || exit 127; \
case \"${LC_ALL:-${LC_CTYPE:-$LANG}}\" in *[Uu][Tt][Ff]*) ;; \
*) LC_ALL=C.UTF-8; export LC_ALL ;; esac; \
exec btop";

pub const EXIT_NOT_FOUND: i32 = 127;
pub const EXIT_SSH_ERROR: i32 = 255;

fn control_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        use std::os::unix::fs::DirBuilderExt;
        // Unix socket paths are limited to ~104 bytes, so keep this short.
        let dir = match dirs::runtime_dir() {
            Some(runtime) => runtime.join("tomega"),
            None => std::env::temp_dir().join(format!("tomega-{}", std::process::id())),
        };
        let _ = std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&dir);
        dir
    })
}

#[derive(Clone, Debug)]
pub struct SshTarget {
    pub session: SshSession,
    control_path: PathBuf,
}

impl SshTarget {
    pub fn new(session: SshSession) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let control_path = control_dir().join(format!("{}-{id}", std::process::id()));
        Self {
            session,
            control_path,
        }
    }

    fn control_option(&self) -> [String; 2] {
        [
            "-o".into(),
            format!("ControlPath={}", self.control_path.display()),
        ]
    }

    fn connection_args(&self) -> Vec<String> {
        let mut args = vec!["-p".to_string(), self.session.port.to_string()];
        let user = self.session.user.trim();
        if !user.is_empty() {
            args.extend(["-l".to_string(), user.to_string()]);
        }
        let identity = self.session.identity_file.trim();
        if !identity.is_empty() {
            args.extend(["-i".to_string(), identity.to_string()]);
        }
        args
    }

    /// Arguments for the interactive login in the main terminal.
    ///
    /// `ControlPersist` matters: without it the main terminal would hang on
    /// logout until btop closed. The short timeout means an orphaned master
    /// goes away by itself if this app is killed.
    pub fn main_args(&self) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "-o".into(),
            "ControlMaster=auto".into(),
            "-o".into(),
            "ControlPersist=30".into(),
        ];
        args.extend(self.control_option());
        args.extend(self.connection_args());
        args.extend(["--".to_string(), self.session.host.trim().to_string()]);
        args
    }

    /// Arguments for running btop over the existing connection. `BatchMode`
    /// makes it fail, rather than show a password prompt in the btop pane, if
    /// the connection has gone away.
    pub fn btop_args(&self) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "-t".into(),
            "-o".into(),
            "ControlMaster=no".into(),
            "-o".into(),
            "BatchMode=yes".into(),
        ];
        args.extend(self.control_option());
        args.extend(self.connection_args());
        args.extend([
            "--".to_string(),
            self.session.host.trim().to_string(),
            format!("sh -lc '{BTOP_SCRIPT}'"),
        ]);
        args
    }

    /// ssh only publishes the socket after authentication has succeeded.
    pub fn is_logged_in(&self) -> bool {
        self.control_path.exists()
    }

    /// Shuts the shared connection down. Blocks only briefly: it talks to a
    /// local socket, not the network.
    pub fn close_master(&self) {
        if !self.control_path.exists() {
            return;
        }
        let mut command = Command::new("ssh");
        command
            .args(["-O", "exit"])
            .args(self.control_option())
            .args(["--", self.session.host.trim()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _ = command.status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> SshTarget {
        SshTarget::new(SshSession {
            host: "example.com".into(),
            user: "root".into(),
            port: 2222,
            identity_file: "~/.ssh/key".into(),
            ..Default::default()
        })
    }

    #[test]
    fn main_args_end_with_host() {
        let args = target().main_args();
        assert_eq!(args.last().unwrap(), "example.com");
        assert_eq!(args[args.len() - 2], "--");
        assert!(args.contains(&"ControlMaster=auto".to_string()));
    }

    #[test]
    fn btop_args_never_prompt_or_become_master() {
        let args = target().btop_args();
        assert!(args.contains(&"BatchMode=yes".to_string()));
        assert!(args.contains(&"ControlMaster=no".to_string()));
        assert!(args.last().unwrap().starts_with("sh -lc '"));
    }

    #[test]
    fn both_commands_share_one_socket() {
        let target = target();
        let find = |args: Vec<String>| {
            args.into_iter()
                .find(|a| a.starts_with("ControlPath="))
                .unwrap()
        };
        assert_eq!(find(target.main_args()), find(target.btop_args()));
    }

    #[test]
    fn empty_optional_fields_are_omitted() {
        let target = SshTarget::new(SshSession {
            host: "h".into(),
            ..Default::default()
        });
        let args = target.main_args();
        assert!(!args.contains(&"-l".to_string()));
        assert!(!args.contains(&"-i".to_string()));
    }

    #[test]
    fn script_has_no_single_quotes() {
        // It is wrapped in single quotes for the remote shell.
        assert!(!BTOP_SCRIPT.contains('\''));
    }
}
