use serde_json::Value;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
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

    let mcp = run_mcp(
        temp.path(),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "codegraph_search",
                "arguments": { "query": "afterDaemonSync" }
            }
        }),
    );
    assert_eq!(mcp["result"]["_meta"]["freshness"]["state"], "current");
    assert_eq!(mcp["result"]["_meta"]["freshness"]["mode"], "daemon");
    assert!(mcp["result"]["_meta"]["freshness"]["daemonPid"]
        .as_u64()
        .is_some());
    assert_eq!(
        mcp["result"]["structuredContent"][0]["symbol"]["name"],
        "afterDaemonSync"
    );

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

    let restarted = run_json(
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
    assert_eq!(restarted["started"], true);
    #[cfg(unix)]
    {
        let broken = temp.path().join("broken.ts");
        fs::write(&broken, "function unreadable() {}\n").unwrap();
        fs::set_permissions(&broken, fs::Permissions::from_mode(0o000)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = run_json(
                temp.path(),
                &["daemon", "status", "--path", temp.path().to_str().unwrap()],
            );
            if !status["running"].as_bool().unwrap() {
                assert_eq!(status["state"]["phase"], "stopped");
                assert!(status["state"]["error"].as_str().is_some());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "daemon did not release its lock after an indexing failure"
            );
            thread::sleep(Duration::from_millis(50));
        }
        fs::set_permissions(&broken, fs::Permissions::from_mode(0o600)).unwrap();
        fs::remove_file(broken).unwrap();
    }

    #[cfg(not(unix))]
    {
        let stopped = run_json(
            temp.path(),
            &["daemon", "stop", "--path", temp.path().to_str().unwrap()],
        );
        assert_eq!(stopped["stopped"], true);
    }
    let recovered = run_json(
        temp.path(),
        &["daemon", "start", "--path", temp.path().to_str().unwrap()],
    );
    assert_eq!(recovered["started"], true);
    let final_stop = run_json(
        temp.path(),
        &["daemon", "stop", "--path", temp.path().to_str().unwrap()],
    );
    assert_eq!(final_stop["stopped"], true);
}

fn run_mcp(current_dir: &Path, request: Value) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_structurely"))
        .args(["serve", "--mcp", "--path", current_dir.to_str().unwrap()])
        .current_dir(current_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        serde_json::to_writer(&mut *stdin, &request).unwrap();
        stdin.write_all(b"\n").unwrap();
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "MCP failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(output.stdout.split(|byte| *byte == b'\n').next().unwrap()).unwrap()
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
