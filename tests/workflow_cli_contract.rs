use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio},
};

#[test]
fn cli_workflows_are_persistent_evidence_backed_and_bounded() {
    let project = fixture();
    run_ok(project.path(), &["init", "."]);

    let research = run_json(
        project.path(),
        &["research", "atomic publication", "--max-files", "2"],
    );
    assert_eq!(research["query"], "atomic publication");
    assert!(research["files"].as_array().unwrap().len() <= 2);
    assert!(
        research["content_findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["path"] == "README.md"),
        "repository content was absent from research: {research}"
    );

    let trace = run_json(
        project.path(),
        &["trace", "publish", "flush", "--depth", "4"],
    );
    assert_eq!(trace["status"], "found");
    let path = trace["path"].as_array().unwrap();
    assert_eq!(path.len(), 2, "unexpected shortest path: {trace}");
    assert_eq!(path[0]["source"]["name"], "publish");
    assert_eq!(path[1]["target"]["name"], "flush");
    for step in path {
        assert!(step["evidence"]["file"].as_str().is_some());
        assert!(step["evidence"]["line"].as_u64().is_some());
    }
    let shallow = run_json(
        project.path(),
        &["trace", "publish", "flush", "--depth", "1"],
    );
    assert_eq!(shallow["status"], "no_path");
    assert!(shallow["guidance"].as_str().unwrap().contains("depth 1"));
    run_fails(
        project.path(),
        &["trace", "publish", "flush", "--depth", "21"],
        "invalid value",
    );

    let workspace = run_json(project.path(), &["workspace", "create", "Launch"]);
    let workspace_id = string(&workspace, "id");
    let session = run_json(
        project.path(),
        &["session", "start", &workspace_id, "Ship atomic writes"],
    );
    let session_id = string(&session, "id");
    run_json(
        project.path(),
        &[
            "session",
            "add",
            &session_id,
            "decision",
            "Use rename-based publication",
        ],
    );
    run_json(
        project.path(),
        &[
            "session",
            "add",
            &session_id,
            "verification",
            "Crash recovery test passed",
        ],
    );
    let recap = run_json(project.path(), &["recap", &session_id]);
    assert_eq!(recap["event_count"], 2);
    assert!(recap["summary"]
        .as_str()
        .unwrap()
        .contains("[decision] Use rename-based publication"));

    let memory = run_json(
        project.path(),
        &[
            "memory",
            "remember",
            &workspace_id,
            "Atomic publication uses rename",
            "--tags",
            "storage,durability",
        ],
    );
    let memory_id = string(&memory, "id");
    let backup = run_json(project.path(), &["state", "backup", "state-backup.db"]);
    assert!(backup["bytes"].as_u64().unwrap() > 0);
    run_json(
        project.path(),
        &["workspace", "create", "Discarded after restore"],
    );
    run_json(
        project.path(),
        &["state", "restore", "state-backup.db", "--force"],
    );
    let workspaces = run_json(project.path(), &["workspace", "list"]);
    assert_eq!(workspaces.as_array().unwrap().len(), 1);
    assert_eq!(workspaces[0]["id"], workspace_id);

    let recalled = run_json(
        project.path(),
        &["memory", "recall", &workspace_id, "rename"],
    );
    assert_eq!(recalled[0]["memory"]["id"], memory_id);
    assert_eq!(
        recalled[0]["memory"]["tags"],
        json!(["storage", "durability"])
    );

    // Every command above used a new process. These reads prove state survived process exits.
    let shown = run_json(project.path(), &["session", "show", &session_id]);
    assert_eq!(shown["events"].as_array().unwrap().len(), 2);
    assert_eq!(shown["recap"]["id"], recap["id"]);
    let ended = run_json(project.path(), &["session", "end", &session_id]);
    assert_eq!(ended["status"], "completed");
    run_fails(
        project.path(),
        &["session", "add", &session_id, "note", "must be rejected"],
        "session is not active",
    );

    let forgotten = run_json(project.path(), &["memory", "forget", &memory_id]);
    assert_eq!(forgotten["forgotten"], true);
    assert!(run_json(
        project.path(),
        &["memory", "recall", &workspace_id, "rename"]
    )
    .as_array()
    .unwrap()
    .is_empty());
    run_fails(
        project.path(),
        &["workspace", "create", ""],
        "workspace name must not be empty",
    );
}

#[test]
fn mcp_workflows_mutate_shared_state_and_report_contract_errors() {
    let project = fixture();
    run_ok(project.path(), &["init", "."]);
    let mut mcp = McpSession::start(project.path());
    mcp.initialize();

    let listed = mcp.request("tools/list", json!({}));
    let names = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    for expected in [
        "structurely_research",
        "structurely_trace",
        "structurely_workspace",
        "structurely_session",
        "structurely_memory",
    ] {
        assert!(names.contains(&expected), "missing MCP workflow {expected}");
    }

    let research = mcp.call(
        "structurely_research",
        json!({"query": "atomic publication", "maxFiles": 1}),
    );
    assert_eq!(research["result"]["isError"], false);
    assert!(
        research["result"]["structuredContent"]["files"]
            .as_array()
            .unwrap()
            .len()
            <= 1
    );
    let trace = mcp.call(
        "structurely_trace",
        json!({"source": "publish", "target": "flush", "depth": 4}),
    );
    assert_eq!(
        trace["result"]["structuredContent"]["status"],
        json!("found")
    );
    assert_eq!(
        trace["result"]["structuredContent"]["path"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let workspace = mcp.call(
        "structurely_workspace",
        json!({"action": "create", "name": "Agents"}),
    );
    let workspace_id = string(&workspace["result"]["structuredContent"], "id");
    let session = mcp.call(
        "structurely_session",
        json!({"action": "start", "workspace": workspace_id, "title": "MCP run"}),
    );
    let session_id = string(&session["result"]["structuredContent"], "id");
    let added = mcp.call(
        "structurely_session",
        json!({
            "action": "add",
            "session": session_id,
            "kind": "finding",
            "body": "README documents atomic publication"
        }),
    );
    assert_eq!(added["result"]["structuredContent"]["sequence"], 1);
    let recap = mcp.call(
        "structurely_session",
        json!({"action": "recap", "session": session_id}),
    );
    assert_eq!(recap["result"]["structuredContent"]["event_count"], 1);

    let remembered = mcp.call(
        "structurely_memory",
        json!({
            "action": "remember",
            "workspace": workspace_id,
            "body": "Use fsync before rename",
            "tags": ["atomic"]
        }),
    );
    let memory_id = string(&remembered["result"]["structuredContent"], "id");
    let recalled = mcp.call(
        "structurely_memory",
        json!({"action": "recall", "workspace": workspace_id, "query": "fsync"}),
    );
    assert_eq!(
        recalled["result"]["structuredContent"][0]["memory"]["id"],
        memory_id
    );

    let invalid_action = mcp.call(
        "structurely_session",
        json!({"action": "explode", "session": session_id}),
    );
    assert_eq!(invalid_action["result"]["isError"], true);
    assert!(text(&invalid_action).contains("unsupported session action"));
    let invalid_limit = mcp.call(
        "structurely_research",
        json!({"query": "atomic", "maxFiles": 101}),
    );
    assert_eq!(invalid_limit["result"]["isError"], true);

    mcp.finish();

    // MCP writes and CLI reads share the same durable state database.
    let shown = run_json(project.path(), &["session", "show", &session_id]);
    assert_eq!(
        shown["events"][0]["body"],
        "README documents atomic publication"
    );
    assert_eq!(
        shown["recap"]["id"],
        recap["result"]["structuredContent"]["id"]
    );
    let recalled = run_json(
        project.path(),
        &["memory", "recall", &workspace_id, "fsync"],
    );
    assert_eq!(recalled[0]["memory"]["id"], memory_id);
}

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("atomic.rs"),
        r#"
pub fn publish() {
    prepare();
}

fn prepare() {
    flush();
}

fn flush() {}
"#,
    )
    .unwrap();
    fs::write(
        temp.path().join("README.md"),
        "# Atomic publication\n\nPublish files atomically with a durable rename.\n",
    )
    .unwrap();
    temp
}

fn run_ok(current_dir: &Path, arguments: &[&str]) -> Output {
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
    output
}

fn run_json(current_dir: &Path, arguments: &[&str]) -> Value {
    let output = run_ok(current_dir, arguments);
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid JSON for {}: {error}\n{}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn run_fails(current_dir: &Path, arguments: &[&str], expected: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_structurely"))
        .args(arguments)
        .current_dir(current_dir)
        .output()
        .unwrap();
    assert!(!output.status.success(), "command unexpectedly succeeded");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(expected),
        "expected {expected:?} in stderr for {}\n{stderr}",
        arguments.join(" ")
    );
}

fn string(value: &Value, field: &str) -> String {
    value[field].as_str().unwrap().to_owned()
}

fn text(response: &Value) -> String {
    response["result"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

struct McpSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpSession {
    fn start(project: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_structurely"))
            .args(["serve", "--mcp", "--path", project.to_str().unwrap()])
            .env(
                "STRUCTURELY_MCP_TOOLS",
                "research,trace,workspace,session,memory",
            )
            .current_dir(project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Self {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            next_id: 1,
        }
    }

    fn initialize(&mut self) {
        let initialized = self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "workflow-contract", "version": "1"}
            }),
        );
        assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let request = json!({
            "jsonrpc": "2.0",
            "id": self.next_id,
            "method": method,
            "params": params
        });
        self.next_id += 1;
        serde_json::to_writer(&mut self.stdin, &request).unwrap();
        self.stdin.write_all(b"\n").unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
    }

    fn finish(self) {
        drop(self.stdin);
        let output = self.child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "MCP failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
