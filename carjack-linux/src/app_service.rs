use std::ffi::{OsStr, OsString};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use eyre::{Result, eyre};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

use carjack::config::AppService as AppServiceConfig;

/// Generous: a service may have a whole OS image to boot before it answers.
const READY_TIMEOUT: Duration = Duration::from_secs(120);
const READY_POLL: Duration = Duration::from_millis(250);

const STOP_TIMEOUT: Duration = Duration::from_secs(30);
/// Granted after `stop` returns, and again after SIGTERM, before the next escalation.
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);
const EXIT_POLL: Duration = Duration::from_millis(100);

/// A running `[app.service]`. Stops on drop.
pub struct AppService {
    config: AppServiceConfig,
    socket: OsString,
    child: Option<Child>,
}

impl AppService {
    /// Spawn `start` against `socket` and wait for `ready`. `None` if nothing is configured.
    pub async fn start(config: &AppServiceConfig, socket: &OsStr) -> Result<Option<Self>> {
        if !config.is_enabled() {
            return Ok(None);
        }

        tracing::info!("Starting app service: {}", config.start.trim());
        // Its own process group, so signalling reaches anything the service forked.
        let child = shell(config.start.trim(), socket)
            .process_group(0)
            .spawn()
            .map_err(|e| eyre!("failed to start app service '{}': {e}", config.start.trim()))?;

        let mut service = AppService {
            config: config.clone(),
            socket: socket.to_owned(),
            child: Some(child),
        };
        service.await_ready().await?;
        Ok(Some(service))
    }

    async fn await_ready(&mut self) -> Result<()> {
        let ready = self.config.ready.trim().to_owned();
        if ready.is_empty() {
            return Ok(());
        }

        tracing::info!("Waiting for app service: {ready}");
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if let Some(status) = self.exited() {
                return Err(eyre!("app service exited before it was ready ({status})"));
            }
            let probe = ready.clone();
            let socket = self.socket.clone();
            let ok = tokio::task::spawn_blocking(move || {
                shell(&probe, &socket)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
            })
            .await
            .map_err(|e| eyre!("app service ready probe panicked: {e}"))?;

            if ok {
                tracing::info!("App service ready");
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(eyre!(
                    "app service not ready after {READY_TIMEOUT:?} ('{ready}' never succeeded)"
                ));
            }
            tokio::time::sleep(READY_POLL).await;
        }
    }

    /// `stop`, then SIGTERM, then SIGKILL. Every step is bounded, so a wedged service delays
    /// shutdown rather than blocking it.
    pub fn stop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let pgid = Pid::from_raw(child.id() as i32);

        let stop = self.config.stop.trim();
        if !stop.is_empty() {
            tracing::info!("Stopping app service: {stop}");
            run_bounded(stop, &self.socket, STOP_TIMEOUT);
            if wait_for_exit(&mut child, EXIT_TIMEOUT) {
                return;
            }
            tracing::warn!("App service still running after '{stop}'; sending SIGTERM");
        }

        let _ = killpg(pgid, Signal::SIGTERM);
        if wait_for_exit(&mut child, EXIT_TIMEOUT) {
            return;
        }

        tracing::warn!("App service ignored SIGTERM; killing");
        let _ = killpg(pgid, Signal::SIGKILL);
        let _ = child.wait();
    }

    fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.as_mut()?.try_wait().ok().flatten()
    }
}

impl Drop for AppService {
    fn drop(&mut self) {
        self.stop();
    }
}

fn shell(command: &str, socket: &OsStr) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(format!("exec {command}"))
        .env("WAYLAND_DISPLAY", socket);
    cmd
}

fn run_bounded(command: &str, socket: &OsStr, timeout: Duration) {
    let mut child = match shell(command, socket).process_group(0).spawn() {
        Ok(child) => child,
        Err(e) => {
            tracing::warn!("failed to run '{command}': {e}");
            return;
        }
    };
    if wait_for_exit(&mut child, timeout) {
        return;
    }
    tracing::warn!("'{command}' still running after {timeout:?}; killing it");
    let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGKILL);
    let _ = child.wait();
}

/// `true` if it exited. Polled rather than `wait()`ed so the timeout holds, and sync so `Drop` can
/// use it.
fn wait_for_exit(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            // Nothing to wait for; treat as gone rather than spinning to the deadline.
            Err(_) => return true,
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(EXIT_POLL);
    }
}
