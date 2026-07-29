use crate::{atomic_file, engine::PROJECT_DIR, Engine};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use serde::Serialize;
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const LOCK_FILE: &str = "daemon.lock";
const STATE_FILE: &str = "daemon.json";
const STOP_FILE: &str = "daemon.stop";

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct DaemonState {
    pub pid: u32,
    pub project: String,
    pub phase: String,
    pub epoch: u64,
    pub updated_unix_ms: u128,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DaemonStatus {
    pub running: bool,
    pub state: Option<DaemonState>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DaemonStart {
    pub started: bool,
    pub status: DaemonStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct DaemonStop {
    pub stopped: bool,
    pub status: DaemonStatus,
}

pub fn start(project: impl AsRef<Path>, debounce: Duration) -> Result<DaemonStart> {
    let project = project_root(project.as_ref())?;
    let existing = status(&project)?;
    if existing.running {
        return Ok(DaemonStart {
            started: false,
            status: existing,
        });
    }
    let executable = std::env::current_exe().context("locate structurely executable")?;
    Command::new(executable)
        .arg("daemon")
        .arg("run")
        .arg("--path")
        .arg(&project)
        .arg("--debounce-ms")
        .arg(debounce.as_millis().max(10).to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn Structurely daemon")?;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let current = status(&project)?;
        if current.running
            && current
                .state
                .as_ref()
                .is_some_and(|state| matches!(state.phase.as_str(), "running" | "degraded"))
        {
            return Ok(DaemonStart {
                started: true,
                status: current,
            });
        }
        if Instant::now() >= deadline {
            bail!("daemon did not acquire its project lock within 5 seconds");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

pub fn stop(project: impl AsRef<Path>) -> Result<DaemonStop> {
    let project = project_root(project.as_ref())?;
    let current = status(&project)?;
    if !current.running {
        return Ok(DaemonStop {
            stopped: false,
            status: current,
        });
    }
    fs::write(daemon_dir(&project).join(STOP_FILE), b"stop\n")
        .context("write daemon stop request")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let current = status(&project)?;
        if !current.running {
            return Ok(DaemonStop {
                stopped: true,
                status: current,
            });
        }
        if Instant::now() >= deadline {
            bail!("daemon did not release its project lock within 5 seconds");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

pub fn status(project: impl AsRef<Path>) -> Result<DaemonStatus> {
    let project = project_root(project.as_ref())?;
    let paths = DaemonPaths::new(&project);
    let lock = open_lock(&paths.lock)?;
    let running = match lock.try_lock_exclusive() {
        Ok(()) => {
            FileExt::unlock(&lock)?;
            false
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => true,
        Err(error) => return Err(error).context("inspect daemon project lock"),
    };
    let state = fs::read(&paths.state)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    Ok(DaemonStatus { running, state })
}

pub fn run(project: impl AsRef<Path>, debounce: Duration) -> Result<()> {
    let project = project_root(project.as_ref())?;
    let paths = DaemonPaths::new(&project);
    let lock = open_lock(&paths.lock)?;
    lock.try_lock_exclusive()
        .context("another Structurely daemon already owns this project")?;
    let _ = fs::remove_file(&paths.stop);

    let stop = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&stop);
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
    let monitor_stop = Arc::clone(&stop);
    let stop_path = paths.stop.clone();
    let monitor = thread::spawn(move || {
        while !monitor_stop.load(Ordering::Relaxed) {
            if stop_path.exists() {
                monitor_stop.store(true, Ordering::Relaxed);
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
    });

    let mut engine = Engine::open_for_daemon(&project)?;
    let initial_epoch = match engine.sync() {
        Ok(report) => {
            write_state(&paths.state, &DaemonState::running(&project, report.epoch))?;
            report.epoch
        }
        Err(error) => {
            let epoch = engine.committed_epoch().unwrap_or(0);
            write_state(
                &paths.state,
                &DaemonState::degraded(&project, epoch, error.to_string()),
            )?;
            epoch
        }
    };
    let state_path = paths.state.clone();
    let watch_stop = Arc::clone(&stop);
    let state_write_error = Arc::new(Mutex::new(None));
    let reconcile_error = Arc::clone(&state_write_error);
    let degraded_error = Arc::clone(&state_write_error);
    let polling_error = Arc::clone(&state_write_error);
    let last_epoch = Arc::new(AtomicU64::new(initial_epoch));
    let reconcile_epoch = Arc::clone(&last_epoch);
    let degraded_epoch = Arc::clone(&last_epoch);
    let polling_epoch = Arc::clone(&last_epoch);
    let mut watch_result = engine.watch_resilient(
        stop.clone(),
        debounce.max(Duration::from_millis(10)),
        || {},
        |report| {
            reconcile_epoch.store(report.epoch, Ordering::Relaxed);
            if let Ok(mut state_error) = reconcile_error.lock() {
                record_state_update(
                    &state_path,
                    &project,
                    report.epoch,
                    &watch_stop,
                    &mut state_error,
                );
            }
        },
        |error| {
            if let Ok(mut state_error) = degraded_error.lock() {
                record_degraded_update(
                    &state_path,
                    &project,
                    degraded_epoch.load(Ordering::Relaxed),
                    error,
                    &watch_stop,
                    &mut state_error,
                );
            }
        },
        |report| {
            polling_epoch.store(report.epoch, Ordering::Relaxed);
            if let Ok(mut state_error) = polling_error.lock() {
                let polling = anyhow::anyhow!(
                    "filesystem watcher unavailable; graph is current via periodic polling"
                );
                record_degraded_update(
                    &state_path,
                    &project,
                    report.epoch,
                    &polling,
                    &watch_stop,
                    &mut state_error,
                );
            }
        },
    );
    if let Ok(mut error) = state_write_error.lock() {
        if let Some(error) = error.take() {
            watch_result = Err(error.context("publish daemon state after index update"));
        }
    }
    stop.store(true, Ordering::Relaxed);
    let _ = monitor.join();
    let _ = fs::remove_file(&paths.stop);
    let final_epoch = engine
        .status()
        .map(|status| status.epoch)
        .unwrap_or(initial_epoch);
    let final_state = match &watch_result {
        Ok(()) => DaemonState::stopped(&project, final_epoch, None),
        Err(error) => DaemonState::stopped(&project, final_epoch, Some(error.to_string())),
    };
    write_state(&paths.state, &final_state)?;
    // Release database and recovery handles before the daemon lock. Once
    // status reports `running: false`, callers may safely remove or reopen the
    // project on platforms with mandatory file locking.
    drop(engine);
    FileExt::unlock(&lock)?;
    watch_result
}

impl DaemonState {
    fn running(project: &Path, epoch: u64) -> Self {
        Self {
            pid: std::process::id(),
            project: project.display().to_string(),
            phase: "running".to_owned(),
            epoch,
            updated_unix_ms: unix_millis(),
            error: None,
        }
    }

    fn stopped(project: &Path, epoch: u64, error: Option<String>) -> Self {
        Self {
            pid: std::process::id(),
            project: project.display().to_string(),
            phase: "stopped".to_owned(),
            epoch,
            updated_unix_ms: unix_millis(),
            error,
        }
    }

    fn degraded(project: &Path, epoch: u64, error: String) -> Self {
        Self {
            pid: std::process::id(),
            project: project.display().to_string(),
            phase: "degraded".to_owned(),
            epoch,
            updated_unix_ms: unix_millis(),
            error: Some(error),
        }
    }
}

struct DaemonPaths {
    lock: PathBuf,
    state: PathBuf,
    stop: PathBuf,
}

impl DaemonPaths {
    fn new(project: &Path) -> Self {
        let directory = daemon_dir(project);
        Self {
            lock: directory.join(LOCK_FILE),
            state: directory.join(STATE_FILE),
            stop: directory.join(STOP_FILE),
        }
    }
}

fn project_root(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    if !absolute.join(PROJECT_DIR).is_dir() {
        bail!(
            "{} is not initialized; run `structurely init {}`",
            absolute.display(),
            absolute.display()
        );
    }
    Ok(absolute.canonicalize().unwrap_or(absolute))
}

fn daemon_dir(project: &Path) -> PathBuf {
    project.join(PROJECT_DIR)
}

fn open_lock(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("open daemon lock {}", path.display()))
}

fn write_state(path: &Path, state: &DaemonState) -> Result<()> {
    atomic_file::write_atomic(path, &serde_json::to_vec_pretty(state)?)
        .with_context(|| format!("publish daemon state {}", path.display()))
}

fn record_state_update(
    path: &Path,
    project: &Path,
    epoch: u64,
    stop: &AtomicBool,
    error: &mut Option<anyhow::Error>,
) {
    if error.is_some() {
        return;
    }
    if let Err(write_error) = write_state(path, &DaemonState::running(project, epoch)) {
        *error = Some(write_error);
        stop.store(true, Ordering::Relaxed);
    }
}

fn record_degraded_update(
    path: &Path,
    project: &Path,
    epoch: u64,
    reconcile_error: &anyhow::Error,
    stop: &AtomicBool,
    error: &mut Option<anyhow::Error>,
) {
    if error.is_some() {
        return;
    }
    if let Err(write_error) = write_state(
        path,
        &DaemonState::degraded(project, epoch, reconcile_error.to_string()),
    ) {
        *error = Some(write_error);
        stop.store(true, Ordering::Relaxed);
    }
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_publication_failure_requests_shutdown_and_is_retained() {
        let directory = tempfile::tempdir().unwrap();
        let state_path = directory.path().join("daemon.json");
        fs::create_dir(&state_path).unwrap();
        fs::write(state_path.join("sentinel"), b"keep").unwrap();
        let stop = AtomicBool::new(false);
        let mut error = None;

        record_state_update(&state_path, directory.path(), 42, &stop, &mut error);

        assert!(stop.load(Ordering::Relaxed));
        let message = format!("{:#}", error.unwrap());
        assert!(message.contains("publish daemon state"));
        assert_eq!(fs::read(state_path.join("sentinel")).unwrap(), b"keep");
    }
}
