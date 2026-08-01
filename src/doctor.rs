use crate::{daemon, dashboard, integrations, Engine};
use anyhow::Result;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CheckLevel {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorCheck {
    pub name: &'static str,
    pub level: CheckLevel,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remedy: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub project: String,
    pub client: &'static str,
    pub healthy: bool,
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    pub fn exit_code(&self) -> i32 {
        if self.healthy {
            0
        } else {
            2
        }
    }
}

pub fn run(
    project: impl AsRef<Path>,
    client: integrations::AgentClient,
    executable: impl AsRef<Path>,
) -> Result<DoctorReport> {
    let requested = project.as_ref();
    let project = match requested.canonicalize() {
        Ok(project) if project.is_dir() => project,
        Ok(project) => {
            return Ok(single_failure(
                project.display().to_string(),
                client,
                "project",
                "The selected path is not a directory.",
                "Choose a repository directory and rerun structurely doctor.",
            ));
        }
        Err(error) => {
            return Ok(single_failure(
                requested.display().to_string(),
                client,
                "project",
                format!("The selected project cannot be opened: {error}"),
                "Check the path and filesystem permissions, then rerun structurely doctor.",
            ));
        }
    };

    let mut checks = Vec::with_capacity(6);
    checks.push(pass("project", "Repository directory is accessible."));

    match Engine::open_read_only(&project).and_then(|engine| engine.status()) {
        Ok(status) => {
            checks.push(pass(
                "index",
                format!(
                    "Graph epoch {} contains {} indexed files.",
                    status.epoch, status.indexed_files
                ),
            ));
            if status.pending_files == 0 {
                checks.push(pass(
                    "freshness",
                    "The committed graph matches the working tree.",
                ));
            } else {
                checks.push(warn(
                    "freshness",
                    format!("{} files are waiting to be indexed.", status.pending_files),
                    "Run structurely sync or restart the daemon.",
                ));
            }
            if let Some(recovery) = status.storage_recovery {
                checks.push(warn(
                    "storage",
                    format!("The index recovered from a storage problem: {recovery}"),
                    "Review the preserved recovery files before removing them.",
                ));
            }
        }
        Err(error) => checks.push(fail(
            "index",
            format!("The project index is not ready: {error}"),
            "Run structurely init, or structurely setup <client>, in this repository.",
        )),
    }

    match daemon::status(&project) {
        Ok(status) if status.running => checks.push(pass(
            "daemon",
            status
                .state
                .map(|state| format!("Background freshness is running (PID {}).", state.pid))
                .unwrap_or_else(|| "Background freshness is running.".to_owned()),
        )),
        Ok(_) => checks.push(fail(
            "daemon",
            "The background indexer is not running.",
            "Run structurely daemon start --path <project>.",
        )),
        Err(error) => checks.push(fail(
            "daemon",
            format!("Daemon health could not be read: {error}"),
            "Initialize the project, then run structurely daemon start.",
        )),
    }

    match integrations::status(&project, client, executable) {
        Ok(report) if report.installed => checks.push(pass(
            "integration",
            format!(
                "The {} project integration points at this binary.",
                report.client
            ),
        )),
        Ok(report) => checks.push(fail(
            "integration",
            format!(
                "The {} project integration is missing or stale.",
                report.client
            ),
            format!(
                "Run structurely integrations install {} --path <project>.",
                report.client
            ),
        )),
        Err(error) => checks.push(fail(
            "integration",
            format!("The coding-agent configuration is invalid: {error}"),
            format!(
                "Repair the configuration, then run structurely setup {}.",
                client_label(client)
            ),
        )),
    }

    match dashboard::status(project.clone()) {
        Ok(Some(status)) if status.running => checks.push(pass(
            "dashboard",
            format!(
                "The optional private bridge is listening at http://{}.",
                status.address
            ),
        )),
        Ok(Some(_)) => checks.push(warn(
            "dashboard",
            "Dashboard control state exists, but the optional bridge is stopped.",
            "Run structurely dashboard serve --path <project>, or remove stale control state.",
        )),
        Ok(None) => checks.push(pass(
            "dashboard",
            "The optional dashboard is not configured; core agent workflows are unaffected.",
        )),
        Err(error) => checks.push(warn(
            "dashboard",
            format!("Optional dashboard state could not be read: {error}"),
            "Run structurely dashboard remove --path <project> to clear stale control state.",
        )),
    }

    let healthy = checks.iter().all(|check| check.level != CheckLevel::Fail);
    Ok(DoctorReport {
        project: project.display().to_string(),
        client: client_label(client),
        healthy,
        checks,
    })
}

fn single_failure(
    project: String,
    client: integrations::AgentClient,
    name: &'static str,
    detail: impl Into<String>,
    remedy: impl Into<String>,
) -> DoctorReport {
    DoctorReport {
        project,
        client: client_label(client),
        healthy: false,
        checks: vec![fail(name, detail, remedy)],
    }
}

fn pass(name: &'static str, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name,
        level: CheckLevel::Pass,
        detail: detail.into(),
        remedy: None,
    }
}

fn warn(name: &'static str, detail: impl Into<String>, remedy: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name,
        level: CheckLevel::Warn,
        detail: detail.into(),
        remedy: Some(remedy.into()),
    }
}

fn fail(name: &'static str, detail: impl Into<String>, remedy: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name,
        level: CheckLevel::Fail,
        detail: detail.into(),
        remedy: Some(remedy.into()),
    }
}

fn client_label(client: integrations::AgentClient) -> &'static str {
    match client {
        integrations::AgentClient::Codex => "codex",
        integrations::AgentClient::Claude => "claude",
        integrations::AgentClient::Cursor => "cursor",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_exit_code_tracks_failures_but_not_warnings() {
        let healthy = DoctorReport {
            project: ".".into(),
            client: "codex",
            healthy: true,
            checks: vec![warn("dashboard", "stopped", "start it")],
        };
        assert_eq!(healthy.exit_code(), 0);

        let failed = DoctorReport {
            healthy: false,
            ..healthy
        };
        assert_eq!(failed.exit_code(), 2);
    }
}
