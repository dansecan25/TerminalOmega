//! Checks GitHub for a newer release, downloads the package for this system
//! and installs it with the package's own `install.sh`.
//!
//! Only the copy that `install.sh` put in place updates itself. A build run
//! from a source checkout is left alone.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use egui::Context;
use serde::Deserialize;

const REPO: &str = "dansecan25/TerminalOmega";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const CHECK_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);
const MAX_PACKAGE_BYTES: u64 = 200 * 1024 * 1024;

/// A downloaded package, ready to install.
#[derive(Clone, Debug)]
pub struct Update {
    pub version: String,
    installer: PathBuf,
}

#[derive(Clone, Debug)]
pub enum Status {
    Idle,
    Checking,
    UpToDate,
    /// Checking or downloading failed, for example while offline. Shown only
    /// in the settings, since the next check usually works.
    CheckFailed(String),
    Downloading(String),
    Ready(Update),
    Installing(String),
    Installed(String),
    InstallFailed(String),
}

fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.trim().trim_start_matches('v').split('.');
    let mut next = || parts.next()?.parse().ok();
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

/// The package name `release.sh` gives this system's build.
fn package_name(version: &str) -> Option<String> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "macos",
        _ => return None,
    };
    // `release.sh` names packages after `uname -m`, which says arm64 on Macs.
    let arch = match (os, std::env::consts::ARCH) {
        ("macos", "aarch64") => "arm64",
        (_, arch) => arch,
    };
    Some(format!("terminal-omega-{version}-{os}-{arch}"))
}

/// Where `install.sh` puts the program.
fn installed_executable() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(if cfg!(target_os = "macos") {
        home.join("Applications/TerminalOmega.app/Contents/MacOS/terminal-omega")
    } else {
        home.join(".local/bin/terminal-omega")
    })
}

/// True if this is the copy `install.sh` put in place.
pub fn can_update() -> bool {
    let canonical = |path: PathBuf| fs::canonicalize(path).ok();
    let running = std::env::current_exe().ok().and_then(canonical);
    let installed = installed_executable().and_then(canonical);
    running.is_some() && running == installed
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(format!("terminal-omega/{CURRENT_VERSION}"))
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(10 * 60)))
        .build()
        .new_agent()
}

fn latest_release(agent: &ureq::Agent) -> Result<Release, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let text = agent
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .and_then(|mut response| response.body_mut().read_to_string())
        .map_err(|e| format!("Could not check for updates: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("Unexpected answer from GitHub: {e}"))
}

fn download(agent: &ureq::Agent, url: &str, to: &Path) -> Result<(), String> {
    let error = |e: &dyn std::fmt::Display| format!("Could not download the update: {e}");
    let mut response = agent.get(url).call().map_err(|e| error(&e))?;
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(MAX_PACKAGE_BYTES)
        .reader();
    let mut file = fs::File::create(to).map_err(|e| error(&e))?;
    io::copy(&mut reader, &mut file).map_err(|e| error(&e))?;
    Ok(())
}

/// Downloads and unpacks the latest release if it is newer than `current`.
fn fetch_update(current: &str, report: &dyn Fn(Status)) -> Result<Option<Update>, String> {
    let agent = agent();
    let release = latest_release(&agent)?;
    let version = release.tag_name.trim_start_matches('v').to_string();
    if !is_newer(&version, current) {
        return Ok(None);
    }
    let name = package_name(&version).ok_or("Updates aren't available for this system")?;
    let file_name = format!("{name}.tar.gz");
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == file_name)
        .ok_or_else(|| format!("Version {version} has no package for this system yet"))?;

    report(Status::Downloading(version.clone()));
    let dir = dirs::cache_dir()
        .ok_or("No cache folder on this system")?
        .join("terminal-omega")
        .join("update");
    // Only one update is kept, so an older download is removed.
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    let archive = dir.join(&file_name);
    download(&agent, &asset.browser_download_url, &archive)?;

    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("Could not unpack the update: {e}"))?;
    let installer = dir.join(&name).join("install.sh");
    if !status.success() || !installer.is_file() {
        return Err("The downloaded update is damaged. It will be downloaded again later.".into());
    }
    let _ = fs::remove_file(&archive);
    Ok(Some(Update { version, installer }))
}

fn install(update: &Update) -> Result<(), String> {
    let output = Command::new("sh")
        .arg(&update.installer)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Could not run the installer: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let reason = stderr.lines().last().unwrap_or("unknown error").trim();
    Err(format!("The update couldn't be installed: {reason}"))
}

pub struct Updater {
    pub status: Status,
    updates: Receiver<Status>,
    sender: Sender<Status>,
    last_check: Option<Instant>,
    /// Hides the update notice in the top bar until the next start.
    pub dismissed: bool,
}

impl Updater {
    pub fn new() -> Self {
        let (sender, updates) = channel();
        Self {
            status: Status::Idle,
            updates,
            sender,
            last_check: None,
            dismissed: false,
        }
    }

    fn busy(&self) -> bool {
        matches!(
            self.status,
            Status::Checking | Status::Downloading(_) | Status::Installing(_)
        )
    }

    /// Runs `work` on a thread, which reports progress through the channel.
    fn run(
        &mut self,
        ctx: &Context,
        first: Status,
        work: impl FnOnce(&dyn Fn(Status)) + Send + 'static,
    ) {
        self.status = first;
        let sender = self.sender.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let report = |status: Status| {
                let _ = sender.send(status);
                ctx.request_repaint();
            };
            work(&report);
        });
    }

    pub fn check_now(&mut self, ctx: &Context) {
        if self.busy() || !can_update() {
            return;
        }
        self.last_check = Some(Instant::now());
        self.run(ctx, Status::Checking, |report| {
            report(match fetch_update(CURRENT_VERSION, report) {
                Ok(Some(update)) => Status::Ready(update),
                Ok(None) => Status::UpToDate,
                Err(err) => Status::CheckFailed(err),
            });
        });
    }

    pub fn install(&mut self, ctx: &Context) {
        let Status::Ready(update) = self.status.clone() else {
            return;
        };
        let installing = Status::Installing(update.version.clone());
        self.run(ctx, installing, move |report| {
            report(match install(&update) {
                Ok(()) => Status::Installed(update.version),
                Err(err) => Status::InstallFailed(err),
            });
        });
    }

    /// Takes progress reports, and starts a check when one is due.
    pub fn poll(&mut self, ctx: &Context, automatic: bool) {
        while let Ok(status) = self.updates.try_recv() {
            self.status = status;
        }
        let waiting = matches!(
            self.status,
            Status::Idle | Status::UpToDate | Status::CheckFailed(_)
        );
        if automatic && waiting {
            let due = self
                .last_check
                .is_none_or(|at| at.elapsed() >= CHECK_INTERVAL);
            if due {
                self.check_now(ctx);
            } else {
                ctx.request_repaint_after(CHECK_INTERVAL);
            }
        }
    }
}

/// Starts the newly installed version. The caller then closes this one.
pub fn restart() -> Result<(), String> {
    let executable = installed_executable().ok_or("No home folder")?;
    let mut command = if cfg!(target_os = "macos") {
        // The bundle, three levels up from the executable.
        let app = executable
            .ancestors()
            .nth(3)
            .ok_or("Unexpected install location")?;
        let mut command = Command::new("open");
        command.arg("-n").arg(app);
        command
    } else {
        Command::new(executable)
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| format!("Could not start the new version: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("v1.10.0", "1.9.3"));
        assert!(is_newer("1.1.1", "1.1.0"));
        assert!(!is_newer("v1.1.0", "1.1.0"));
        assert!(!is_newer("1.0.9", "1.1.0"));
    }

    #[test]
    fn ignores_unusual_tags() {
        assert!(!is_newer("v2.0.0-beta", "1.0.0"));
        assert!(!is_newer("nightly", "1.0.0"));
        assert!(!is_newer("2.0", "1.0.0"));
    }

    #[test]
    #[ignore = "downloads the latest release from GitHub"]
    fn downloads_and_unpacks_the_latest_release() {
        let update = fetch_update("0.0.1", &|_| {}).unwrap().unwrap();
        assert!(update.installer.is_file());
        let package = update.installer.parent().unwrap();
        assert!(package.join("install.sh").is_file());
        assert!(fetch_update("999.0.0", &|_| {}).unwrap().is_none());
    }

    #[test]
    fn package_names_match_release_sh() {
        let name = package_name("1.2.3").unwrap();
        assert!(name.starts_with("terminal-omega-1.2.3-"));
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            assert_eq!(name, "terminal-omega-1.2.3-linux-x86_64");
        }
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            assert_eq!(name, "terminal-omega-1.2.3-macos-arm64");
        }
    }
}
