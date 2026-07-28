use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

#[test]
fn daemon_start_status_catch_up_and_stop_are_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("main.ts"), "function before() {}\n").unwrap();
    run(temp.path(), &["init", temp.path().to_str().unwrap()]);

    let started = run_json(
        temp.path(),
        &[
            "daemon",
            "start",
            "--path",
            temp.path().to_str().unwrap(),
            "--debounce-ms",
            "25",
        ],
    );
    assert_eq!(started["started"], true);
    assert_eq!(started["status"]["running"], true);
    let initial_epoch = started["status"]["state"]["epoch"].as_u64().unwrap();

    let duplicate = run_json(
        temp.path(),
        &["daemon", "start", "--path", temp.path().to_str().unwrap()],
    );
    assert_eq!(duplicate["started"], false);
    assert_eq!(duplicate["status"]["running"], true);

    fs::write(
        temp.path().join("main.ts"),
        "function afterDaemonSync() {}\n",
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = run_json(
            temp.path(),
            &["daemon", "status", "--path", temp.path().to_str().unwrap()],
        );
        if status["state"]["epoch"].as_u64().unwrap_or(0) > initial_epoch {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "daemon did not publish changed source"
        );
        thread::sleep(Duration::from_millis(50));
    }

    let stopped = run_json(
        temp.path(),
        &["daemon", "stop", "--path", temp.path().to_str().unwrap()],
    );
    assert_eq!(stopped["stopped"], true);
    assert_eq!(stopped["status"]["running"], false);
    assert_eq!(stopped["status"]["state"]["phase"], "stopped");

    let duplicate_stop = run_json(
        temp.path(),
        &["daemon", "stop", "--path", temp.path().to_str().unwrap()],
    );
    assert_eq!(duplicate_stop["stopped"], false);
}

fn run_json(current_dir: &Path, arguments: &[&str]) -> Value {
    let output = run(current_dir, arguments);
    serde_json::from_slice(&output).unwrap()
}

fn run(current_dir: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = Command::new(env!("CARGO_BIN_EXE_structurely"))
        .args(arguments)
        .current_dir(current_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "command failed: {}\nstdout: {}\nstderr: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
