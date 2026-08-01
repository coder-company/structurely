use serde_json::Value;
use std::{path::Path, process::Command};
use tempfile::TempDir;

fn cli(project: &Path, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_structurely"))
        .args(arguments)
        .current_dir(project)
        .env("STRUCTURELY_DASHBOARD_SETUP", "skip")
        .output()
        .unwrap()
}

#[test]
fn doctor_reports_actionable_failure_then_verified_readiness() {
    let project = TempDir::new().unwrap();
    std::fs::write(project.path().join("main.rs"), "fn main() {}\n").unwrap();

    let before = cli(project.path(), &["doctor", "."]);
    assert_eq!(before.status.code(), Some(2));
    let before: Value = serde_json::from_slice(&before.stdout).unwrap();
    assert_eq!(before["healthy"], false);
    assert!(before["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check["name"] == "index" && check["level"] == "fail"));

    let setup = cli(project.path(), &["setup", "codex", "."]);
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );

    let after = cli(project.path(), &["doctor", "."]);
    assert!(
        after.status.success(),
        "{}",
        String::from_utf8_lossy(&after.stderr)
    );
    let after: Value = serde_json::from_slice(&after.stdout).unwrap();
    assert_eq!(after["healthy"], true);
    for required in [
        "project",
        "index",
        "freshness",
        "daemon",
        "integration",
        "dashboard",
    ] {
        assert!(after["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["name"] == required));
    }

    let stop = cli(project.path(), &["daemon", "stop", "--path", "."]);
    assert!(stop.status.success());
}
