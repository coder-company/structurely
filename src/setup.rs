use crate::{
    daemon::{self, DaemonStart},
    engine::PROJECT_DIR,
    integrations::{self, AgentClient, IntegrationReport},
    Engine, IndexReport,
};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::{path::Path, time::Duration};

#[derive(Debug, Serialize)]
pub struct SetupReport {
    pub project: String,
    pub index: IndexReport,
    pub daemon: DaemonStart,
    pub integration: IntegrationReport,
    pub ready: bool,
}

pub fn run(
    project: impl AsRef<Path>,
    client: AgentClient,
    executable: impl AsRef<Path>,
    replace_codegraph: bool,
) -> Result<SetupReport> {
    let project = project
        .as_ref()
        .canonicalize()
        .with_context(|| format!("resolve project {}", project.as_ref().display()))?;
    let index = if project.join(PROJECT_DIR).is_dir() {
        Engine::open(&project)?.sync()?
    } else {
        let (engine, report) = Engine::init(&project)?;
        drop(engine);
        report
    };

    let daemon = daemon::start(&project, Duration::from_millis(250))?;
    let integration = if replace_codegraph {
        integrations::replace_codegraph(&project, client, executable.as_ref())
    } else {
        integrations::install(&project, client, executable.as_ref())
    };
    let integration = match integration {
        Ok(report) => report,
        Err(error) => {
            let _ = daemon::stop(&project);
            return Err(error).context("configure coding agent");
        }
    };
    let verified = if replace_codegraph {
        integrations::status_codegraph_replacement(&project, client, executable)?
    } else {
        integrations::status(&project, client, executable)?
    };
    if !verified.installed {
        let _ = daemon::stop(&project);
        bail!("coding-agent configuration did not pass post-install verification");
    }
    if !daemon::status(&project)?.running {
        bail!("background indexer did not pass post-install verification");
    }

    Ok(SetupReport {
        project: project.display().to_string(),
        index,
        daemon,
        integration,
        ready: true,
    })
}
