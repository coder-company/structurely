use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};
use structurely::Engine;

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn dashboard_bridge_pairs_once_and_enforces_auth_and_origins() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("main.rs"),
        "fn publish_atomically() {}\n",
    )
    .unwrap();
    Engine::init(project.path()).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_structurely"))
        .args([
            "dashboard",
            "serve",
            "--path",
            project.path().to_str().unwrap(),
            "--port",
            "0",
            "--allow-origin",
            "https://console.example",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut ready_line = String::new();
    reader.read_line(&mut ready_line).unwrap();
    let ready: Value = serde_json::from_str(&ready_line).unwrap();
    let address = ready["address"].as_str().unwrap();
    let pairing_code = ready["pairing_code"].as_str().unwrap();
    let guard = ChildGuard(child);

    wait_until_ready(address);
    let health = request(address, "GET", "/api/v1/health", &[], "");
    assert_eq!(health.0, 200);
    assert_eq!(request(address, "GET", "/api/v1/status", &[], "").0, 401);
    assert_eq!(
        request(
            address,
            "GET",
            "/api/v1/health",
            &[("Origin", "https://evil.example")],
            "",
        )
        .0,
        403
    );

    let pair = request(
        address,
        "POST",
        "/api/v1/pair",
        &[
            ("Origin", "https://console.example"),
            ("Content-Type", "application/json"),
        ],
        &format!("{{\"code\":\"{pairing_code}\"}}"),
    );
    assert_eq!(pair.0, 200, "{}", pair.1);
    let token = serde_json::from_str::<Value>(&pair.1).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        request(
            address,
            "POST",
            "/api/v1/pair",
            &[
                ("Origin", "https://console.example"),
                ("Content-Type", "application/json"),
            ],
            &format!("{{\"code\":\"{pairing_code}\"}}"),
        )
        .0,
        409
    );

    let authorization = format!("Bearer {token}");
    let status = request(
        address,
        "GET",
        "/api/v1/status",
        &[
            ("Origin", "https://console.example"),
            ("Authorization", &authorization),
        ],
        "",
    );
    assert_eq!(status.0, 200, "{}", status.1);
    assert_eq!(
        serde_json::from_str::<Value>(&status.1).unwrap()["indexed_files"],
        1
    );
    assert_eq!(
        request(
            address,
            "OPTIONS",
            "/api/v1/status",
            &[
                ("Origin", "https://console.example"),
                ("Access-Control-Request-Private-Network", "true"),
            ],
            "",
        )
        .0,
        204
    );
    assert_eq!(request(address, "GET", "/", &[], "").0, 200);

    let status_report = cli_json(&[
        "dashboard",
        "status",
        "--path",
        project.path().to_str().unwrap(),
    ]);
    assert_eq!(status_report["running"], true);
    assert_eq!(status_report["generation"], 1);
    assert!(status_report["pairing_code"].is_null());

    let rotated = cli_json(&[
        "dashboard",
        "rotate-token",
        "--path",
        project.path().to_str().unwrap(),
    ]);
    assert_eq!(rotated["generation"], 2);
    let next_code = rotated["pairing_code"].as_str().unwrap();
    assert_eq!(
        request(
            address,
            "GET",
            "/api/v1/status",
            &[("Authorization", &authorization)],
            "",
        )
        .0,
        401
    );
    assert_eq!(
        request(
            address,
            "POST",
            "/api/v1/pair",
            &[("Content-Type", "application/json")],
            &format!("{{\"code\":\"{next_code}\"}}"),
        )
        .0,
        200
    );
    let stopped = cli_json(&[
        "dashboard",
        "stop",
        "--path",
        project.path().to_str().unwrap(),
    ]);
    assert_eq!(stopped["stopped"], true);
    drop(guard);
}

#[test]
fn dashboard_export_contains_only_static_shell_assets() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("dashboard");
    let status = Command::new(env!("CARGO_BIN_EXE_structurely"))
        .args(["dashboard", "export", output.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success());
    let mut names = fs::read_dir(&output)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        [
            "_headers",
            "app.js",
            "index.html",
            "styles.css",
            "vercel.json"
        ]
    );
    for name in names {
        let contents = fs::read(output.join(name)).unwrap();
        assert!(!contents
            .windows(b"pairing_code".len())
            .any(|window| window == b"pairing_code"));
        assert!(!contents
            .windows(b"Authorization: Bearer".len())
            .any(|window| window == b"Authorization: Bearer"));
    }
}

fn wait_until_ready(address: &str) {
    for _ in 0..100 {
        if TcpStream::connect(address).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("dashboard bridge did not accept connections");
}

fn cli_json(arguments: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_structurely"))
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn request(
    address: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    )
    .unwrap();
    for (name, value) in headers {
        write!(stream, "{name}: {value}\r\n").unwrap();
    }
    write!(stream, "\r\n{body}").unwrap();
    stream.flush().unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let status = head
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    (status, body.to_owned())
}
