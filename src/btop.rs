//! The resource monitor pane that follows the selected main tab.

use egui::Context;

use crate::ssh::{BTOP_SCRIPT, EXIT_NOT_FOUND, EXIT_SSH_ERROR, SshTarget};
use crate::terminal::{Launch, Terminal};
use crate::theme::Theme;

pub enum BtopState {
    /// The main terminal hasn't finished logging in yet.
    WaitingForLogin,
    Running(Terminal),
    NotInstalled,
    Failed(String),
    /// btop was closed by the user.
    Stopped,
    /// The main terminal's session is gone.
    SessionEnded,
}

pub struct BtopPane {
    target: Option<SshTarget>,
    pub state: BtopState,
}

impl BtopPane {
    /// `target` is `None` when monitoring this machine.
    pub fn new(target: Option<SshTarget>) -> Self {
        Self {
            target,
            state: BtopState::WaitingForLogin,
        }
    }

    /// Runs btop again, e.g. after the user has installed it.
    pub fn refresh(&mut self) {
        self.state = BtopState::WaitingForLogin;
    }

    pub fn session_ended(&mut self) {
        self.state = BtopState::SessionEnded;
    }

    fn launch(&self) -> Launch {
        match &self.target {
            Some(target) => Launch::command("ssh", target.btop_args()),
            None => Launch::command("sh", vec!["-lc".into(), BTOP_SCRIPT.into()]),
        }
    }

    pub fn poll(&mut self, ctx: &Context, theme: &Theme, scrollback: usize) {
        match &mut self.state {
            BtopState::WaitingForLogin => {
                let ready = self.target.as_ref().is_none_or(SshTarget::is_logged_in);
                if ready {
                    self.state = match Terminal::spawn(ctx, self.launch(), scrollback) {
                        Ok(terminal) => BtopState::Running(terminal),
                        Err(err) => BtopState::Failed(format!("Could not start btop: {err}")),
                    };
                } else {
                    // Nothing else wakes the UI while the user types a password.
                    ctx.request_repaint_after(std::time::Duration::from_millis(250));
                }
            }
            BtopState::Running(terminal) => {
                terminal.poll(ctx, theme);
                if let Some(status) = terminal.exit {
                    let is_remote = self.target.is_some();
                    // Whatever it printed last is usually the actual reason.
                    let output = terminal.last_output(4);
                    self.state = match classify_exit(status.code(), is_remote) {
                        BtopState::Failed(message) if !output.is_empty() => {
                            BtopState::Failed(format!("{message}\n{output}"))
                        }
                        state => state,
                    };
                }
            }
            _ => {}
        }
    }
}

fn classify_exit(code: Option<i32>, is_remote: bool) -> BtopState {
    match code {
        Some(EXIT_NOT_FOUND) => BtopState::NotInstalled,
        Some(EXIT_SSH_ERROR) if is_remote => {
            BtopState::Failed("Lost the connection to the server.".into())
        }
        Some(0) => BtopState::Stopped,
        Some(code) => BtopState::Failed(format!("btop exited with code {code}.")),
        None => BtopState::Stopped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_btop_is_detected() {
        assert!(matches!(
            classify_exit(Some(127), true),
            BtopState::NotInstalled
        ));
        assert!(matches!(
            classify_exit(Some(127), false),
            BtopState::NotInstalled
        ));
    }

    #[test]
    fn ssh_failure_is_only_meaningful_remotely() {
        let BtopState::Failed(remote) = classify_exit(Some(255), true) else {
            panic!()
        };
        assert!(remote.contains("connection"));
        let BtopState::Failed(local) = classify_exit(Some(255), false) else {
            panic!()
        };
        assert!(local.contains("255"));
    }

    #[test]
    fn quitting_btop_is_not_an_error() {
        assert!(matches!(classify_exit(Some(0), true), BtopState::Stopped));
    }
}
