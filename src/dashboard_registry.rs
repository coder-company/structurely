//! User-local project registry for the single dashboard bridge.

use crate::{atomic_file, Engine};
use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    fs::OpenOptions,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const REGISTRY_VERSION: u32 = 1;
const MAX_PROJECTS: usize = 128;
const REGISTRY_FILE: &str = "projects.json";
const REGISTRY_LOCK: &str = "projects.lock";
const ORIGINS_FILE: &str = "dashboard-origins.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegisteredProject {
    pub id: String,
    pub name: String,
    pub path: String,
    pub registered_unix_ms: u128,
    pub last_opened_unix_ms: u128,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProjectEntry {
    #[serde(flatten)]
    pub project: RegisteredProject,
    pub available: bool,
    pub active: bool,
    pub health: Option<serde_json::Value>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryDocument {
    version: u32,
    active_project: Option<String>,
    projects: Vec<RegisteredProject>,
}

impl Default for RegistryDocument {
    fn default() -> Self {
        Self {
            version: REGISTRY_VERSION,
            active_project: None,
            projects: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DashboardRegistry {
    directory: PathBuf,
}

impl DashboardRegistry {
    pub fn open_default() -> Result<Self> {
        Self::open_at(default_structurely_home()?)
    }

    pub fn open_at(directory: impl Into<PathBuf>) -> Result<Self> {
        let registry = Self {
            directory: directory.into(),
        };
        fs::create_dir_all(&registry.directory).with_context(|| {
            format!(
                "create dashboard registry directory {}",
                registry.directory.display()
            )
        })?;
        restrict_directory_permissions(&registry.directory)?;
        Ok(registry)
    }

    pub fn register(&self, project: impl AsRef<Path>) -> Result<RegisteredProject> {
        let canonical = project
            .as_ref()
            .canonicalize()
            .with_context(|| format!("resolve dashboard project {}", project.as_ref().display()))?;
        Engine::open_read_only(&canonical).context("register an initialized project")?;
        self.mutate(|document| {
            let now = unix_time_ms();
            let path = canonical.display().to_string();
            let id = project_id(&canonical);
            let name = canonical
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .unwrap_or("Project")
                .to_owned();
            let project = if let Some(existing) = document
                .projects
                .iter_mut()
                .find(|candidate| candidate.id == id)
            {
                existing.path = path;
                existing.name = name;
                existing.last_opened_unix_ms = now;
                existing.clone()
            } else {
                anyhow::ensure!(
                    document.projects.len() < MAX_PROJECTS,
                    "dashboard registry supports at most {MAX_PROJECTS} projects"
                );
                let project = RegisteredProject {
                    id: id.clone(),
                    name,
                    path,
                    registered_unix_ms: now,
                    last_opened_unix_ms: now,
                };
                document.projects.push(project.clone());
                project
            };
            document.active_project = Some(id);
            Ok(project)
        })
    }

    pub fn list(&self) -> Result<Vec<ProjectEntry>> {
        let document = self.read()?;
        let mut projects = document
            .projects
            .into_iter()
            .map(|project| {
                let status = Engine::open_read_only(Path::new(&project.path))
                    .and_then(|engine| engine.status());
                let (available, health, error) = match status {
                    Ok(status) => (true, serde_json::to_value(status).ok(), None),
                    Err(error) => (false, None, Some(error.to_string())),
                };
                ProjectEntry {
                    available,
                    active: document.active_project.as_deref() == Some(project.id.as_str()),
                    health,
                    error,
                    project,
                }
            })
            .collect::<Vec<_>>();
        projects.sort_by(|left, right| {
            right
                .project
                .last_opened_unix_ms
                .cmp(&left.project.last_opened_unix_ms)
                .then_with(|| left.project.name.cmp(&right.project.name))
        });
        Ok(projects)
    }

    pub fn resolve(&self, id: Option<&str>) -> Result<RegisteredProject> {
        let mut document = self.read()?;
        let selected = id
            .map(str::to_owned)
            .or_else(|| document.active_project.clone())
            .context("dashboard has no active project; register one first")?;
        let project = document
            .projects
            .iter_mut()
            .find(|project| project.id == selected)
            .context("dashboard project is not registered")?;
        anyhow::ensure!(
            initialized_project(Path::new(&project.path)),
            "dashboard project is unavailable: {}",
            project.path
        );
        Ok(project.clone())
    }

    pub fn activate(&self, id: &str) -> Result<RegisteredProject> {
        self.mutate(|document| {
            let project = document
                .projects
                .iter_mut()
                .find(|project| project.id == id)
                .context("dashboard project is not registered")?;
            anyhow::ensure!(
                initialized_project(Path::new(&project.path)),
                "dashboard project is unavailable: {}",
                project.path
            );
            project.last_opened_unix_ms = unix_time_ms();
            let project = project.clone();
            document.active_project = Some(project.id.clone());
            Ok(project)
        })
    }

    pub fn remove(&self, id: &str) -> Result<RegisteredProject> {
        self.mutate(|document| {
            let index = document
                .projects
                .iter()
                .position(|project| project.id == id)
                .context("dashboard project is not registered")?;
            let removed = document.projects.remove(index);
            if document.active_project.as_deref() == Some(id) {
                document.active_project =
                    document.projects.first().map(|project| project.id.clone());
            }
            Ok(removed)
        })
    }

    pub fn path(&self) -> PathBuf {
        self.directory.join(REGISTRY_FILE)
    }

    pub fn control_path(&self, name: &str) -> PathBuf {
        self.directory.join(name)
    }

    pub fn dashboard_origins(&self) -> Result<Vec<String>> {
        let path = self.directory.join(ORIGINS_FILE);
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("parse dashboard origins"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error).context("read dashboard origins"),
        }
    }

    pub fn remember_dashboard_origin(&self, origin: &str) -> Result<()> {
        let mut origins = self.dashboard_origins()?;
        if !origins.iter().any(|existing| existing == origin) {
            origins.push(origin.to_owned());
            origins.sort();
            origins.dedup();
            anyhow::ensure!(origins.len() <= 16, "too many dashboard deployment origins");
            let path = self.directory.join(ORIGINS_FILE);
            atomic_file::write_atomic(&path, &serde_json::to_vec_pretty(&origins)?)?;
            restrict_file_permissions(&path)?;
        }
        Ok(())
    }

    fn read(&self) -> Result<RegistryDocument> {
        read_document(&self.path())
    }

    fn mutate<T>(&self, operation: impl FnOnce(&mut RegistryDocument) -> Result<T>) -> Result<T> {
        let lock_path = self.directory.join(REGISTRY_LOCK);
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .context("open dashboard registry lock")?;
        lock.lock_exclusive().context("lock dashboard registry")?;
        let mut document = self.read()?;
        let result = operation(&mut document)?;
        write_document(&self.path(), &document)?;
        FileExt::unlock(&lock).context("unlock dashboard registry")?;
        Ok(result)
    }
}

fn read_document(path: &Path) -> Result<RegistryDocument> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RegistryDocument::default())
        }
        Err(error) => return Err(error).context("read dashboard project registry"),
    };
    let document: RegistryDocument =
        serde_json::from_slice(&bytes).context("parse dashboard project registry")?;
    anyhow::ensure!(
        document.version == REGISTRY_VERSION,
        "unsupported dashboard registry version {}",
        document.version
    );
    anyhow::ensure!(
        document.projects.len() <= MAX_PROJECTS,
        "dashboard registry exceeds the project limit"
    );
    Ok(document)
}

fn write_document(path: &Path, document: &RegistryDocument) -> Result<()> {
    atomic_file::write_atomic(path, &serde_json::to_vec_pretty(document)?)
        .context("write dashboard project registry")?;
    restrict_file_permissions(path)
}

fn initialized_project(path: &Path) -> bool {
    path.is_dir() && Engine::open_read_only(path).is_ok()
}

fn project_id(path: &Path) -> String {
    let digest = blake3::hash(path.as_os_str().as_encoded_bytes()).to_hex();
    format!("project_{}", &digest[..16])
}

fn default_structurely_home() -> Result<PathBuf> {
    if let Some(path) = env::var_os("STRUCTURELY_HOME") {
        let path = PathBuf::from(path);
        anyhow::ensure!(
            path.is_absolute(),
            "STRUCTURELY_HOME must be an absolute path"
        );
        return Ok(path);
    }
    let home = env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .context("user home directory is not set")?;
    Ok(PathBuf::from(home).join(".structurely"))
}

fn unix_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(unix)]
fn restrict_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .context("restrict dashboard registry directory permissions")
}

#[cfg(not(unix))]
fn restrict_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .context("restrict dashboard registry permissions")
}

#[cfg(not(unix))]
fn restrict_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn initialized(name: &str) -> tempfile::TempDir {
        let project = tempdir().unwrap();
        fs::write(
            project.path().join("main.rs"),
            format!("fn {name}() {{}}\n"),
        )
        .unwrap();
        Engine::init(project.path()).unwrap();
        project
    }

    #[test]
    fn registration_is_stable_active_and_project_local() {
        let home = tempdir().unwrap();
        let first = initialized("first");
        let second = initialized("second");
        let registry = DashboardRegistry::open_at(home.path()).unwrap();
        let first_record = registry.register(first.path()).unwrap();
        let first_again = registry.register(first.path()).unwrap();
        assert_eq!(first_record.id, first_again.id);
        let second_record = registry.register(second.path()).unwrap();
        assert_eq!(registry.resolve(None).unwrap().id, second_record.id);
        registry.activate(&first_record.id).unwrap();
        assert_eq!(registry.resolve(None).unwrap().id, first_record.id);
        assert_eq!(registry.list().unwrap().len(), 2);
        registry.remove(&first_record.id).unwrap();
        assert_eq!(registry.resolve(None).unwrap().id, second_record.id);
    }

    #[test]
    fn stale_projects_are_reported_and_cannot_be_selected() {
        let home = tempdir().unwrap();
        let project = initialized("gone");
        let registry = DashboardRegistry::open_at(home.path()).unwrap();
        let record = registry.register(project.path()).unwrap();
        drop(project);
        let entries = registry.list().unwrap();
        assert!(!entries[0].available);
        assert!(registry.activate(&record.id).is_err());
    }
}
