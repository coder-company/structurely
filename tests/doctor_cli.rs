use serde_json::Value;
use std::{
    fs::File,
    io::{Read, Seek},
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

fn cli(project: &Path, arguments: &[&str]) -> std::process::Output {
    let mut stdout = tempfile::tempfile().unwrap();
    let mut stderr = tempfile::tempfile().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_structurely"))
        .args(arguments)
        .current_dir(project)
        .env("STRUCTURELY_DASHBOARD_SETUP", "skip")
        .stdout(stdout.try_clone().unwrap())
        .stderr(stderr.try_clone().unwrap())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if child.try_wait().unwrap().is_some() {
            let status = child.wait().unwrap();
            return std::process::Output {
                status,
                stdout: read_capture(&mut stdout),
                stderr: read_capture(&mut stderr),
            };
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            let captured_stdout = read_capture(&mut stdout);
            let captured_stderr = read_capture(&mut stderr);
            panic!(
                "command timed out: {}\nstdout: {}\nstderr: {}",
                arguments.join(" "),
                String::from_utf8_lossy(&captured_stdout),
                String::from_utf8_lossy(&captured_stderr)
            );
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn read_capture(file: &mut File) -> Vec<u8> {
    file.rewind().unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    bytes
}

#[test]
fn doctor_reports_actionable_failure_then_verified_readiness() {
    let project = TempDir::new().unwrap();
    std::fs::write(project.path().join("main.rs"), "fn main() {}\n").unwrap();

    eprintln!("doctor acceptance: checking an uninitialized project");
    let before = cli(project.path(), &["doctor", "."]);
    assert_eq!(before.status.code(), Some(2));
    let before: Value = serde_json::from_slice(&before.stdout).unwrap();
    assert_eq!(before["healthy"], false);
    assert!(before["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check["name"] == "index" && check["level"] == "fail"));

    eprintln!("doctor acceptance: setting up codex");
    let setup = cli(project.path(), &["setup", "codex", "."]);
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );

    eprintln!("doctor acceptance: checking configured readiness");
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

    eprintln!("doctor acceptance: stopping freshness daemon");
    let stop = cli(project.path(), &["daemon", "stop", "--path", "."]);
    assert!(stop.status.success());
}
