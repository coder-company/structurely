use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

const EXPECTED_TOOLS: [&str; 8] = [
    "codegraph_explore",
    "codegraph_node",
    "codegraph_search",
    "codegraph_callers",
    "codegraph_callees",
    "codegraph_impact",
    "codegraph_status",
    "codegraph_files",
];

#[test]
fn mcp_stdio_preserves_the_codegraph_agent_contract() {
    let temp = tempfile::tempdir().unwrap();
    copy_fixture(&fixture_root(), temp.path());
    run(temp.path(), &["init", temp.path().to_str().unwrap()]);

    let mut session = McpSession::start(temp.path());
    let initialized = session.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "structurely-contract", "version": "1" }
        }),
    );
    assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");

    let tools = session.request("tools/list", json!({}));
    let names = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    for expected in EXPECTED_TOOLS {
        assert!(names.contains(&expected), "missing MCP tool {expected}");
    }

    let exact = session.call("codegraph_search", json!({"query": "showUser"}));
    assert!(text(&exact).contains("showUser"));
    assert!(exact["result"]["structuredContent"].is_array());

    let ambiguous = session.call("codegraph_search", json!({"query": "duplicate"}));
    assert!(
        text(&ambiguous).matches("duplicate").count() >= 2,
        "ambiguous definitions were collapsed: {}",
        text(&ambiguous)
    );

    let callers = session.call(
        "codegraph_callers",
        json!({"symbol": "showUser", "limit": 20}),
    );
    assert!(text(&callers).contains("showUser"));

    let node = session.call(
        "codegraph_node",
        json!({"file": "handlers.ts", "offset": 1, "limit": 6}),
    );
    assert!(text(&node).contains("showUser"));
    assert!(!text(&node).contains("export function duplicate"));

    let explore = session.call(
        "codegraph_explore",
        json!({"query": "registerRoutes showUser", "maxFiles": 5}),
    );
    assert!(text(&explore).contains("registerRoutes"));
    assert!(text(&explore).contains("authorize"));
    assert!(text(&explore).contains("showUser"));
    assert!(text(&explore).contains("dispatchReady"));
    assert!(text(&explore).contains("ready"));
    assert!(!text(&explore).contains("api.py"));

    let files = session.call(
        "codegraph_files",
        json!({"pattern": "*.ts", "includeMetadata": true}),
    );
    assert!(text(&files).contains("handlers.ts"));
    assert!(text(&files).contains("routes.ts"));

    let missing = session.call("codegraph_search", json!({}));
    assert_eq!(missing["result"]["isError"], true);
    let invalid = session.call(
        "codegraph_search",
        json!({"query": "showUser", "limit": 1_000_000}),
    );
    assert_eq!(invalid["result"]["isError"], true);

    session.finish();
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/differential/mcp-1.5.0")
}

fn copy_fixture(source: &Path, destination: &Path) {
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() != "scenarios.json" {
            fs::copy(entry.path(), destination.join(entry.file_name())).unwrap();
        }
    }
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
                "CODEGRAPH_MCP_TOOLS",
                "explore,node,search,callers,callees,impact,status,files",
            )
            .current_dir(project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
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

fn run(current_dir: &Path, arguments: &[&str]) {
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
}
