use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};
use structurely::{state::StateStore, Engine};

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
    let state = StateStore::open(project.path()).unwrap();
    let workspace = state.create_workspace("Release work").unwrap();
    let session = state
        .create_session(&workspace.id, "Verify publication")
        .unwrap();
    drop(state);
    let mut state = StateStore::open(project.path()).unwrap();
    state
        .append_event(&session.id, "decision", "Keep publication atomic.")
        .unwrap();
    state
        .remember(
            &workspace.id,
            "Publication uses an atomic rename.",
            &["storage".to_owned()],
        )
        .unwrap();
    drop(state);

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
    let status_json = serde_json::from_str::<Value>(&status.1).unwrap();
    assert_eq!(status_json["indexed_files"], 1);
    assert_eq!(status_json["symbols"], 2);
    assert_eq!(status_json["relationships"], 1);

    let post_json = |path: &str, body: &str| {
        let response = request(
            address,
            "POST",
            path,
            &[
                ("Origin", "https://console.example"),
                ("Authorization", &authorization),
                ("Content-Type", "application/json"),
            ],
            body,
        );
        assert_eq!(response.0, 200, "{path}: {}", response.1);
        serde_json::from_str::<Value>(&response.1).unwrap()
    };
    let search = post_json("/api/v1/search", r#"{"query":"publish","limit":10}"#);
    assert_eq!(search[0]["symbol"]["name"], "publish_atomically");
    assert!(search[0]["score"].is_number());
    let research = post_json(
        "/api/v1/research",
        r#"{"query":"atomic publication","max_files":8}"#,
    );
    assert!(research["graph_epoch"].is_number());
    assert!(research["symbol_findings"].is_array());
    assert!(research["content_findings"].is_array());
    assert!(research["files"].is_array());
    assert!(post_json(
        "/api/v1/impact",
        r#"{"symbol":"publish_atomically","depth":2}"#
    )
    .is_array());
    let trace = post_json(
        "/api/v1/trace",
        r#"{"source":"publish_atomically","target":"publish_atomically","depth":4}"#,
    );
    assert!(trace["status"].is_string());
    assert!(trace["path"].is_array());
    let workspaces = request(
        address,
        "GET",
        "/api/v1/workspaces",
        &[
            ("Origin", "https://console.example"),
            ("Authorization", &authorization),
        ],
        "",
    );
    assert_eq!(workspaces.0, 200, "{}", workspaces.1);
    assert_eq!(
        serde_json::from_str::<Value>(&workspaces.1).unwrap()[0]["id"],
        workspace.id
    );
    let sessions = post_json("/api/v1/sessions", r#"{"limit":20}"#);
    assert_eq!(sessions[0]["id"], session.id);
    let memory = post_json(
        "/api/v1/memory",
        &format!(
            r#"{{"workspace":"{}","query":"atomic rename","limit":10}}"#,
            workspace.id
        ),
    );
    assert_eq!(memory[0]["memory"]["workspace_id"], workspace.id);
    let recap = post_json(
        "/api/v1/recap",
        &format!(r#"{{"session":"{}"}}"#, session.id),
    );
    assert_eq!(recap["session_id"], session.id);
    assert_eq!(recap["event_count"], 1);
    let created_workspace = post_json("/api/v1/workspaces", r#"{"name":"Dashboard workflow"}"#);
    let created_session = post_json(
        "/api/v1/sessions/create",
        &format!(
            r#"{{"workspace":"{}","title":"Connected workflow"}}"#,
            created_workspace["id"].as_str().unwrap()
        ),
    );
    let created_event = post_json(
        "/api/v1/sessions/events",
        &format!(
            r#"{{"session":"{}","kind":"decision","body":"Keep evidence visible."}}"#,
            created_session["id"].as_str().unwrap()
        ),
    );
    assert_eq!(created_event["sequence"], 1);
    let completed = post_json(
        "/api/v1/sessions/complete",
        &format!(
            r#"{{"session":"{}"}}"#,
            created_session["id"].as_str().unwrap()
        ),
    );
    assert_eq!(completed["status"], "completed");
    let remembered = post_json(
        "/api/v1/memories",
        &format!(
            r#"{{"workspace":"{}","body":"Evidence stays inspectable.","tags":["dashboard","ux"]}}"#,
            created_workspace["id"].as_str().unwrap()
        ),
    );
    assert_eq!(remembered["tags"][0], "dashboard");
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
    for _ in 0..8 {
        assert_eq!(
            request(
                address,
                "POST",
                "/api/v1/pair",
                &[("Content-Type", "application/json")],
                "{\"code\":\"invalid\"}",
            )
            .0,
            401
        );
    }
    assert_eq!(
        request(
            address,
            "POST",
            "/api/v1/pair",
            &[("Content-Type", "application/json")],
            &format!("{{\"code\":\"{next_code}\"}}"),
        )
        .0,
        429
    );
    let reconnected = cli_json(&[
        "dashboard",
        "reconnect",
        "--path",
        project.path().to_str().unwrap(),
    ]);
    assert_eq!(reconnected["generation"], 3);
    let next_code = reconnected["pairing_code"].as_str().unwrap();
    let repaired = request(
        address,
        "POST",
        "/api/v1/pair",
        &[("Content-Type", "application/json")],
        &format!("{{\"code\":\"{next_code}\"}}"),
    );
    assert_eq!(repaired.0, 200);
    let next_token = serde_json::from_str::<Value>(&repaired.1).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let next_authorization = format!("Bearer {next_token}");
    for _ in 0..120 {
        assert_eq!(
            request(
                address,
                "GET",
                "/api/v1/status",
                &[("Authorization", &next_authorization)],
                "",
            )
            .0,
            200
        );
    }
    assert_eq!(
        request(
            address,
            "GET",
            "/api/v1/status",
            &[("Authorization", &next_authorization)],
            "",
        )
        .0,
        429
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
            "favicon.svg",
            "index.html",
            "styles.css",
            "theme.js",
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

#[cfg(unix)]
#[test]
fn dashboard_deploy_uses_provider_cli_and_verifies_static_shell() {
    use std::os::unix::fs::PermissionsExt;

    for provider in ["vercel", "cloudflare"] {
        let directory = tempfile::tempdir().unwrap();
        let bin = directory.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let log = directory.path().join("provider.log");
        let provider_cli = if provider == "vercel" {
            "vercel"
        } else {
            "wrangler"
        };
        fs::write(
            bin.join(provider_cli),
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'test 1.0'; exit 0; fi\nprintf '%s\\n' \"$*\" > '{}'\necho 'https://private-console.example'\n",
                log.display()
            ),
        )
        .unwrap();
        fs::write(
            bin.join("curl"),
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'curl test'; fi\nexit 0\n",
        )
        .unwrap();
        for executable in [bin.join(provider_cli), bin.join("curl")] {
            fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = Command::new(env!("CARGO_BIN_EXE_structurely"))
            .args([
                "dashboard",
                "deploy",
                provider,
                "--project-name",
                "private-console",
            ])
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["provider"], provider);
        assert_eq!(report["verified"], true);
        assert_eq!(report["project_data_uploaded"], false);
        let arguments = fs::read_to_string(&log).unwrap();
        assert!(arguments.contains("private-console"));
        assert!(arguments.contains("structurely-dashboard-"));
        let temporary = arguments
            .split_whitespace()
            .find(|argument| argument.contains("structurely-dashboard-"))
            .unwrap();
        assert!(
            !std::path::Path::new(temporary).exists(),
            "deployment staging directory was not cleaned"
        );
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
