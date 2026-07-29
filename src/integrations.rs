use crate::atomic_file;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};
use toml_edit::{value, Array, DocumentMut, Item, Table};

const SERVER_NAME: &str = "structurely";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentClient {
    Codex,
    Claude,
    Cursor,
}

impl AgentClient {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "codex" => Ok(Self::Codex),
            "claude" | "claude-code" => Ok(Self::Claude),
            "cursor" => Ok(Self::Cursor),
            _ => bail!("unsupported coding-agent client `{value}`; use codex, claude, or cursor"),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Cursor => "cursor",
        }
    }

    fn config_path(self, project: &Path) -> PathBuf {
        match self {
            Self::Codex => project.join(".codex/config.toml"),
            Self::Claude => project.join(".mcp.json"),
            Self::Cursor => project.join(".cursor/mcp.json"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct IntegrationReport {
    pub client: &'static str,
    pub config: String,
    pub installed: bool,
    pub changed: bool,
}

pub fn install(
    project: impl AsRef<Path>,
    client: AgentClient,
    executable: impl AsRef<Path>,
) -> Result<IntegrationReport> {
    let project = canonical_project(project.as_ref())?;
    let executable = absolute_executable(executable.as_ref())?;
    let path = client.config_path(&project);
    let changed = match client {
        AgentClient::Codex => install_codex(&path, &project, &executable)?,
        AgentClient::Claude | AgentClient::Cursor => install_json(&path, &project, &executable)?,
    };
    Ok(report(client, path, true, changed))
}

pub fn uninstall(project: impl AsRef<Path>, client: AgentClient) -> Result<IntegrationReport> {
    let project = canonical_project(project.as_ref())?;
    let path = client.config_path(&project);
    let changed = match client {
        AgentClient::Codex => uninstall_codex(&path)?,
        AgentClient::Claude | AgentClient::Cursor => uninstall_json(&path)?,
    };
    Ok(report(client, path, false, changed))
}

pub fn status(
    project: impl AsRef<Path>,
    client: AgentClient,
    executable: impl AsRef<Path>,
) -> Result<IntegrationReport> {
    let project = canonical_project(project.as_ref())?;
    let executable = absolute_executable(executable.as_ref())?;
    let path = client.config_path(&project);
    let installed = match client {
        AgentClient::Codex => codex_matches(&path, &project, &executable)?,
        AgentClient::Claude | AgentClient::Cursor => json_matches(&path, &project, &executable)?,
    };
    Ok(report(client, path, installed, false))
}

fn install_json(path: &Path, project: &Path, executable: &Path) -> Result<bool> {
    let mut document = read_json_object(path)?;
    let before = document.clone();
    let servers = document
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .context("`mcpServers` must be a JSON object")?;
    servers.insert(
        SERVER_NAME.to_owned(),
        json!({
            "command": executable,
            "args": ["serve", "--mcp", "--path", project],
            "env": {}
        }),
    );
    if document == before {
        return Ok(false);
    }
    write_atomic(path, &serde_json::to_vec_pretty(&document)?)?;
    Ok(true)
}

fn uninstall_json(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let mut document = read_json_object(path)?;
    let removed = document
        .get_mut("mcpServers")
        .and_then(Value::as_object_mut)
        .and_then(|servers| servers.remove(SERVER_NAME))
        .is_some();
    if !removed {
        return Ok(false);
    }
    if document
        .get("mcpServers")
        .and_then(Value::as_object)
        .is_some_and(Map::is_empty)
    {
        document.remove("mcpServers");
    }
    if document.is_empty() {
        fs::remove_file(path)?;
        remove_empty_parent(path);
    } else {
        write_atomic(path, &serde_json::to_vec_pretty(&document)?)?;
    }
    Ok(true)
}

fn json_matches(path: &Path, project: &Path, executable: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let document = read_json_object(path)?;
    Ok(document["mcpServers"][SERVER_NAME]
        == json!({
            "command": executable,
            "args": ["serve", "--mcp", "--path", project],
            "env": {}
        }))
}

fn install_codex(path: &Path, project: &Path, executable: &Path) -> Result<bool> {
    let mut document = read_toml(path)?;
    let before = document.to_string();
    let server = codex_server(&mut document);
    server["command"] = value(executable.display().to_string());
    let mut arguments = Array::new();
    for argument in [
        "serve".to_owned(),
        "--mcp".to_owned(),
        "--path".to_owned(),
        project.display().to_string(),
    ] {
        arguments.push(argument);
    }
    server["args"] = value(arguments);
    let rendered = document.to_string();
    if rendered == before {
        return Ok(false);
    }
    write_atomic(path, rendered.as_bytes())?;
    Ok(true)
}

fn uninstall_codex(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let mut document = read_toml(path)?;
    let removed = document
        .get_mut("mcp_servers")
        .and_then(Item::as_table_mut)
        .and_then(|servers| servers.remove(SERVER_NAME))
        .is_some();
    if !removed {
        return Ok(false);
    }
    if document
        .get("mcp_servers")
        .and_then(Item::as_table)
        .is_some_and(Table::is_empty)
    {
        document.remove("mcp_servers");
    }
    if document.is_empty() {
        fs::remove_file(path)?;
        remove_empty_parent(path);
    } else {
        write_atomic(path, document.to_string().as_bytes())?;
    }
    Ok(true)
}

fn codex_matches(path: &Path, project: &Path, executable: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let document = read_toml(path)?;
    let Some(server) = document
        .get("mcp_servers")
        .and_then(Item::as_table)
        .and_then(|servers| servers.get(SERVER_NAME))
        .and_then(Item::as_table)
    else {
        return Ok(false);
    };
    let command_matches = server["command"].as_str() == Some(&executable.display().to_string());
    let arguments = server["args"]
        .as_array()
        .map(|array| {
            array
                .iter()
                .filter_map(|item| item.as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(command_matches
        && arguments
            == [
                "serve",
                "--mcp",
                "--path",
                project.to_str().unwrap_or_default(),
            ])
}

fn codex_server(document: &mut DocumentMut) -> &mut Table {
    if !document.get("mcp_servers").is_some_and(Item::is_table) {
        document["mcp_servers"] = Item::Table(Table::new());
    }
    let servers = document["mcp_servers"].as_table_mut().unwrap();
    if !servers.get(SERVER_NAME).is_some_and(Item::is_table) {
        servers.insert(SERVER_NAME, Item::Table(Table::new()));
    }
    servers
        .get_mut(SERVER_NAME)
        .and_then(Item::as_table_mut)
        .unwrap()
}

fn read_json_object(path: &Path) -> Result<Map<String, Value>> {
    if !path.exists() {
        return Ok(Map::new());
    }
    serde_json::from_slice::<Value>(&fs::read(path)?)
        .with_context(|| format!("parse {}", path.display()))?
        .as_object()
        .cloned()
        .context("coding-agent configuration must be a JSON object")
}

fn read_toml(path: &Path) -> Result<DocumentMut> {
    if !path.exists() {
        return Ok(DocumentMut::new());
    }
    fs::read_to_string(path)?
        .parse()
        .with_context(|| format!("parse {}", path.display()))
}

fn canonical_project(project: &Path) -> Result<PathBuf> {
    project
        .canonicalize()
        .with_context(|| format!("resolve project {}", project.display()))
}

fn absolute_executable(executable: &Path) -> Result<PathBuf> {
    if executable.is_absolute() {
        Ok(executable.to_owned())
    } else {
        Ok(std::env::current_dir()?.join(executable))
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    atomic_file::write_atomic(path, bytes)
        .with_context(|| format!("publish coding-agent configuration {}", path.display()))
}

fn remove_empty_parent(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = fs::remove_dir(parent);
    }
}

fn report(client: AgentClient, path: PathBuf, installed: bool, changed: bool) -> IntegrationReport {
    IntegrationReport {
        client: client.name(),
        config: path.display().to_string(),
        installed,
        changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_clients_install_idempotently_and_uninstall_only_their_entry() {
        for client in [AgentClient::Codex, AgentClient::Claude, AgentClient::Cursor] {
            let temp = tempfile::tempdir().unwrap();
            let executable = temp.path().join("bin/structurely");
            let path = client.config_path(temp.path());
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            match client {
                AgentClient::Codex => {
                    fs::write(&path, "model = \"gpt-test\"\n").unwrap();
                }
                _ => {
                    fs::write(
                        &path,
                        r#"{"theme":"dark","mcpServers":{"unrelated":{"command":"other"}}}"#,
                    )
                    .unwrap();
                }
            }

            assert!(install(temp.path(), client, &executable).unwrap().changed);
            assert!(!install(temp.path(), client, &executable).unwrap().changed);
            assert!(status(temp.path(), client, &executable).unwrap().installed);
            assert!(uninstall(temp.path(), client).unwrap().changed);
            assert!(!uninstall(temp.path(), client).unwrap().changed);

            let content = fs::read_to_string(&path).unwrap();
            match client {
                AgentClient::Codex => assert!(content.contains("model = \"gpt-test\"")),
                _ => {
                    let document: Value = serde_json::from_str(&content).unwrap();
                    assert_eq!(document["theme"], "dark");
                    assert_eq!(document["mcpServers"]["unrelated"]["command"], "other");
                }
            }
            assert!(!content.contains("structurely"));
        }
    }
}
