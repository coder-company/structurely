use serde_json::Value;
#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    io::{Read, Seek},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[test]
fn daemon_start_status_catch_up_and_stop_are_idempotent() {
    let temp = tempfile::tempdir().unwrap().keep();
    fs::write(temp.join("main.ts"), "function before() {}\n").unwrap();
    run(&temp, &["init", temp.to_str().unwrap()]);
    eprintln!("daemon acceptance: initialized");

    let started = run_json(
        &temp,
        &[
            "daemon",
            "start",
            "--path",
            temp.to_str().unwrap(),
            "--debounce-ms",
            "25",
        ],
    );
    assert_eq!(started["started"], true);
    assert_eq!(started["status"]["running"], true);
    eprintln!("daemon acceptance: started");
    let initial_epoch = started["status"]["state"]["epoch"].as_u64().unwrap();

    let duplicate = run_json(
        &temp,
        &["daemon", "start", "--path", temp.to_str().unwrap()],
    );
    assert_eq!(duplicate["started"], false);
    assert_eq!(duplicate["status"]["running"], true);
    eprintln!("daemon acceptance: duplicate start rejected");

    fs::write(temp.join("main.ts"), "function afterDaemonSync() {}\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = run_json(
            &temp,
            &["daemon", "status", "--path", temp.to_str().unwrap()],
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
    eprintln!("daemon acceptance: source synchronized");

    #[cfg(unix)]
    {
        let mcp = run_mcp(
            &temp,
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
    }

    let stopped = run_json(&temp, &["daemon", "stop", "--path", temp.to_str().unwrap()]);
    assert_eq!(stopped["stopped"], true);
    assert_eq!(stopped["status"]["running"], false);
    assert_eq!(stopped["status"]["state"]["phase"], "stopped");
    eprintln!("daemon acceptance: stopped");

    let duplicate_stop = run_json(&temp, &["daemon", "stop", "--path", temp.to_str().unwrap()]);
    assert_eq!(duplicate_stop["stopped"], false);

    let restarted = run_json(
        &temp,
        &[
            "daemon",
            "start",
            "--path",
            temp.to_str().unwrap(),
            "--debounce-ms",
            "25",
        ],
    );
    assert_eq!(restarted["started"], true);
    eprintln!("daemon acceptance: restarted");
    #[cfg(unix)]
    {
        let broken = temp.join("broken.ts");
        fs::write(&broken, "function unreadable() {}\n").unwrap();
        fs::set_permissions(&broken, fs::Permissions::from_mode(0o000)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = run_json(
                &temp,
                &["daemon", "status", "--path", temp.to_str().unwrap()],
            );
            if status["state"]["phase"] == "degraded" {
                assert_eq!(status["running"], true);
                assert!(status["state"]["error"].as_str().is_some());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "daemon did not report a degraded indexing state"
            );
            thread::sleep(Duration::from_millis(50));
        }
        fs::set_permissions(&broken, fs::Permissions::from_mode(0o600)).unwrap();
        fs::remove_file(broken).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = run_json(
                &temp,
                &["daemon", "status", "--path", temp.to_str().unwrap()],
            );
            if status["state"]["phase"] == "running" {
                assert_eq!(status["running"], true);
                assert!(status["state"]["error"].is_null());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "daemon did not recover after the source became readable"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    let duplicate = run_json(
        &temp,
        &["daemon", "start", "--path", temp.to_str().unwrap()],
    );
    assert_eq!(duplicate["started"], false);
    eprintln!("daemon acceptance: duplicate restart rejected");
    let final_stop = run_json(&temp, &["daemon", "stop", "--path", temp.to_str().unwrap()]);
    assert_eq!(final_stop["stopped"], true);
    eprintln!("daemon acceptance: final stop completed");
    #[cfg(unix)]
    fs::remove_dir_all(temp).unwrap();
}

#[cfg(unix)]
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
    let mut response = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut response)
        .unwrap();
    let _ = child.kill();
    child.wait().unwrap();
    serde_json::from_str(&response).unwrap()
}

fn run_json(current_dir: &Path, arguments: &[&str]) -> Value {
    let output = run(current_dir, arguments);
    serde_json::from_slice(&output).unwrap()
}

fn run(current_dir: &Path, arguments: &[&str]) -> Vec<u8> {
    let mut stdout = tempfile::tempfile().unwrap();
    let mut stderr = tempfile::tempfile().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_structurely"))
        .args(arguments)
        .current_dir(current_dir)
        .stdout(stdout.try_clone().unwrap())
        .stderr(stderr.try_clone().unwrap())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let status = child.wait().unwrap();
            let stdout = read_capture(&mut stdout);
            let stderr = read_capture(&mut stderr);
            panic!(
                "command timed out with {status}: {}\nstdout: {}\nstderr: {}",
                arguments.join(" "),
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            );
        }
        thread::sleep(Duration::from_millis(25));
    };
    let stdout = read_capture(&mut stdout);
    let stderr = read_capture(&mut stderr);
    assert!(
        status.success(),
        "command failed: {}\nstdout: {}\nstderr: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    stdout
}

fn read_capture(file: &mut fs::File) -> Vec<u8> {
    file.rewind().unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    bytes
}
