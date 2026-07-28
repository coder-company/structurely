use crate::{engine::PROJECT_DIR, Engine};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use serde::Serialize;
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
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
        if current.running {
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

    let mut engine = Engine::open(&project)?;
    let report = engine.sync()?;
    write_state(&paths.state, &DaemonState::running(&project, report.epoch))?;
    let state_path = paths.state.clone();
    let watch_result = engine.watch(
        stop.clone(),
        debounce.max(Duration::from_millis(10)),
        |report| {
            let _ = write_state(&state_path, &DaemonState::running(&project, report.epoch));
        },
    );
    stop.store(true, Ordering::Relaxed);
    let _ = monitor.join();
    let _ = fs::remove_file(&paths.stop);
    let final_epoch = engine
        .status()
        .map(|status| status.epoch)
        .unwrap_or(report.epoch);
    let final_state = match &watch_result {
        Ok(()) => DaemonState::stopped(&project, final_epoch, None),
        Err(error) => DaemonState::stopped(&project, final_epoch, Some(error.to_string())),
    };
    write_state(&paths.state, &final_state)?;
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
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(state)?)
        .with_context(|| format!("write daemon state {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("publish daemon state {}", path.display()))
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
