use crate::Engine;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::Path;

pub fn serve_stdio(root: &Path) -> Result<()> {
    let engine = Engine::open(root)?;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.len() > 4 * 1024 * 1024 {
            continue;
        }
        let request: Value = serde_json::from_str(&line).context("parse MCP JSON-RPC request")?;
        if let Some(response) = handle(&engine, request)? {
            serde_json::to_writer(&mut stdout, &response)?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

fn handle(engine: &Engine, request: Value) -> Result<Option<Value>> {
    let id = request.get("id").cloned();
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if id.is_none() {
        return Ok(None);
    }
    let result = match method {
        "initialize" => json!({
            "protocolVersion": "2025-03-26",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "structurely", "version": env!("CARGO_PKG_VERSION") }
        }),
        "tools/list" => json!({ "tools": tool_definitions() }),
        "tools/call" => {
            let params = request.get("params").cloned().unwrap_or(Value::Null);
            call_tool(engine, &params)?
        }
        "ping" => json!({}),
        _ => {
            return Ok(Some(json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32601, "message": format!("Method not found: {method}") }
            })));
        }
    };
    Ok(Some(
        json!({ "jsonrpc": "2.0", "id": id, "result": result }),
    ))
}

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "codegraph_search",
            "description": "Quick symbol search by name. Returns locations only.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "kind": { "type": "string" },
                    "limit": { "type": "number", "default": 10 },
                    "projectPath": { "type": "string" }
                },
                "required": ["query"]
            },
            "annotations": read_only_annotations()
        }),
        json!({
            "name": "codegraph_explore",
            "description": "Explore relevant symbols with source and call context.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "maxFiles": { "type": "number", "default": 12 },
                    "projectPath": { "type": "string" }
                },
                "required": ["query"]
            },
            "annotations": read_only_annotations()
        }),
        relationship_tool("codegraph_callers", "List functions that call a symbol."),
        relationship_tool("codegraph_callees", "List functions that a symbol calls."),
        json!({
            "name": "codegraph_status",
            "description": "Index health check.",
            "inputSchema": {
                "type": "object",
                "properties": { "projectPath": { "type": "string" } }
            },
            "annotations": read_only_annotations()
        }),
        json!({
            "name": "codegraph_files",
            "description": "List indexed files with language and symbol counts.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "pattern": { "type": "string" },
                    "format": { "type": "string", "enum": ["tree", "flat", "grouped"] },
                    "includeMetadata": { "type": "boolean", "default": true },
                    "projectPath": { "type": "string" }
                }
            },
            "annotations": read_only_annotations()
        }),
        json!({
            "name": "codegraph_node",
            "description": "Read a file or inspect a symbol with optional source.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": { "type": "string" },
                    "includeCode": { "type": "boolean", "default": false },
                    "file": { "type": "string" },
                    "offset": { "type": "number" },
                    "limit": { "type": "number" },
                    "symbolsOnly": { "type": "boolean", "default": false },
                    "line": { "type": "number" },
                    "projectPath": { "type": "string" }
                }
            },
            "annotations": read_only_annotations()
        }),
    ]
}

fn relationship_tool(name: &str, description: &str) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": {
                "symbol": { "type": "string" },
                "file": { "type": "string" },
                "limit": { "type": "number", "default": 20 },
                "projectPath": { "type": "string" }
            },
            "required": ["symbol"]
        },
        "annotations": read_only_annotations()
    })
}

fn read_only_annotations() -> Value {
    json!({
        "readOnlyHint": true,
        "destructiveHint": false,
        "idempotentHint": true,
        "openWorldHint": false
    })
}

fn call_tool(engine: &Engine, params: &Value) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let payload = match name {
        "codegraph_search" => {
            let query = arguments
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let limit = number_argument(&arguments, "limit", 10);
            serde_json::to_value(engine.search(query, limit)?)?
        }
        "codegraph_explore" => {
            let query = arguments
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let max_files = number_argument(&arguments, "maxFiles", 12);
            serde_json::to_value(engine.explore(query, max_files)?)?
        }
        "codegraph_callers" => {
            let symbol = arguments
                .get("symbol")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let file = arguments.get("file").and_then(Value::as_str);
            let limit = number_argument(&arguments, "limit", 20);
            serde_json::to_value(engine.callers_named(symbol, file, limit)?)?
        }
        "codegraph_callees" => {
            let symbol = arguments
                .get("symbol")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let file = arguments.get("file").and_then(Value::as_str);
            let limit = number_argument(&arguments, "limit", 20);
            serde_json::to_value(engine.callees_named(symbol, file, limit)?)?
        }
        "codegraph_status" => serde_json::to_value(engine.status()?)?,
        "codegraph_files" => {
            let path = arguments.get("path").and_then(Value::as_str);
            let pattern = arguments.get("pattern").and_then(Value::as_str);
            let matcher = pattern
                .map(globset::Glob::new)
                .transpose()?
                .map(|glob| glob.compile_matcher());
            let files = engine
                .files()?
                .into_iter()
                .filter(|file| path.is_none_or(|prefix| file.path.starts_with(prefix)))
                .filter(|file| {
                    matcher
                        .as_ref()
                        .is_none_or(|glob| glob.is_match(&file.path))
                })
                .collect::<Vec<_>>();
            serde_json::to_value(files)?
        }
        "codegraph_node" => {
            let symbol = arguments.get("symbol").and_then(Value::as_str);
            let file = arguments.get("file").and_then(Value::as_str);
            let include_code = arguments
                .get("includeCode")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let offset = arguments
                .get("offset")
                .and_then(Value::as_u64)
                .map(|value| value as usize);
            let limit = arguments
                .get("limit")
                .and_then(Value::as_u64)
                .map(|value| value as usize);
            let symbols_only = arguments
                .get("symbolsOnly")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            serde_json::to_value(engine.node(
                symbol,
                file,
                include_code,
                offset,
                limit,
                symbols_only,
            )?)?
        }
        _ => {
            return Ok(
                json!({ "content": [{ "type": "text", "text": format!("Unknown tool: {name}") }], "isError": true }),
            )
        }
    };
    Ok(json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&payload)? }],
        "structuredContent": payload,
        "isError": false
    }))
}

fn number_argument(arguments: &Value, name: &str, default: usize) -> usize {
    arguments
        .get(name)
        .and_then(Value::as_u64)
        .map(|value| value.clamp(1, 100) as usize)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn compatibility_tools_accept_names_and_return_source() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("main.ts"),
            "function caller() { callee(); }\nfunction callee() { return 1; }\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();

        let names: Vec<_> = tool_definitions()
            .into_iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .collect();
        for expected in [
            "codegraph_status",
            "codegraph_files",
            "codegraph_node",
            "codegraph_explore",
        ] {
            assert!(names.contains(&expected.to_owned()));
        }

        let callees = call_tool(
            &engine,
            &json!({ "name": "codegraph_callees", "arguments": { "symbol": "caller" } }),
        )
        .unwrap();
        assert_eq!(callees["structuredContent"][0]["symbol"]["name"], "callee");

        let explored = call_tool(
            &engine,
            &json!({ "name": "codegraph_explore", "arguments": { "query": "callee" } }),
        )
        .unwrap();
        assert!(explored["structuredContent"][0]["source"]
            .as_str()
            .unwrap()
            .contains("function callee"));
    }
}
