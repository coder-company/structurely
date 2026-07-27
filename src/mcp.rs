use crate::Engine;
use anyhow::{bail, ensure, Result};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::Path;

pub fn serve_stdio(root: &Path) -> Result<()> {
    let mut engine = Engine::open(root)?;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.len() > 4 * 1024 * 1024 {
            write_response(
                &mut stdout,
                &json_rpc_error(Value::Null, -32600, "Request exceeds the 4 MiB limit"),
            )?;
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => {
                write_response(
                    &mut stdout,
                    &json_rpc_error(Value::Null, -32700, &format!("Parse error: {error}")),
                )?;
                continue;
            }
        };
        match handle(&mut engine, request) {
            Ok(Some(response)) => write_response(&mut stdout, &response)?,
            Ok(None) => {}
            Err(error) => write_response(
                &mut stdout,
                &json_rpc_error(Value::Null, -32603, &format!("Internal error: {error}")),
            )?,
        }
    }
    Ok(())
}

fn write_response(writer: &mut impl Write, response: &Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, response)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn json_rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

fn handle(engine: &mut Engine, request: Value) -> Result<Option<Value>> {
    let id = request.get("id").cloned();
    if !request.is_object()
        || request.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || request.get("method").and_then(Value::as_str).is_none()
    {
        return Ok(Some(json_rpc_error(
            id.unwrap_or(Value::Null),
            -32600,
            "Invalid Request",
        )));
    }
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
        "tools/list" => json!({ "tools": enabled_tool_definitions(
            std::env::var("CODEGRAPH_MCP_TOOLS").ok().as_deref()
        ) }),
        "tools/call" => {
            let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Ok(Some(json_rpc_error(
                    id.unwrap_or(Value::Null),
                    -32602,
                    "Invalid params: expected an object",
                )));
            }
            match call_tool(engine, &params) {
                Ok(response) => response,
                Err(error) => json!({
                    "content": [{ "type": "text", "text": error.to_string() }],
                    "isError": true
                }),
            }
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
                    "kind": {
                        "type": "string",
                        "enum": ["function", "method", "class", "interface", "type", "variable", "route", "component"]
                    },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 10 },
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
                    "maxFiles": { "type": "integer", "minimum": 1, "maximum": 100, "default": 12 },
                    "projectPath": { "type": "string" }
                },
                "required": ["query"]
            },
            "annotations": read_only_annotations()
        }),
        relationship_tool("codegraph_callers", "List functions that call a symbol."),
        relationship_tool("codegraph_callees", "List functions that a symbol calls."),
        json!({
            "name": "codegraph_impact",
            "description": "List symbols affected by changing a symbol.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": { "type": "string" },
                    "file": { "type": "string" },
                    "depth": { "type": "integer", "minimum": 1, "maximum": 20, "default": 2 },
                    "projectPath": { "type": "string" }
                },
                "required": ["symbol"]
            },
            "annotations": read_only_annotations()
        }),
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
                    "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 1000 },
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
                    "offset": { "type": "integer", "minimum": 0, "maximum": 2000 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 2000 },
                    "symbolsOnly": { "type": "boolean", "default": false },
                    "line": { "type": "number" },
                    "projectPath": { "type": "string" }
                }
            },
            "annotations": read_only_annotations()
        }),
    ]
}

fn enabled_tool_definitions(configured: Option<&str>) -> Vec<Value> {
    let configured = configured.unwrap_or("explore");
    let enabled = configured
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| {
            name.strip_prefix("codegraph_")
                .unwrap_or(name)
                .to_lowercase()
        })
        .collect::<std::collections::HashSet<_>>();
    tool_definitions()
        .into_iter()
        .filter(|tool| {
            tool["name"]
                .as_str()
                .and_then(|name| name.strip_prefix("codegraph_"))
                .is_some_and(|name| enabled.contains(name))
        })
        .collect()
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
                "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
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

fn call_tool(engine: &mut Engine, params: &Value) -> Result<Value> {
    let name = required_string(params, "name")?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    ensure!(arguments.is_object(), "`arguments` must be an object");
    if let Some(project_path) = arguments.get("projectPath").and_then(Value::as_str) {
        let mut project_engine = Engine::open(project_path)?;
        project_engine.sync()?;
        return dispatch_tool(&project_engine, name, &arguments);
    }
    engine.sync()?;
    dispatch_tool(engine, name, &arguments)
}

fn dispatch_tool(engine: &Engine, name: &str, arguments: &Value) -> Result<Value> {
    let payload = match name {
        "codegraph_search" => {
            let query = required_string(arguments, "query")?;
            let limit = number_argument(arguments, "limit", 10, 100)?;
            let kind = match arguments.get("kind") {
                Some(value) => Some(
                    parse_symbol_kind(
                        value
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("`kind` must be a string"))?,
                    )
                    .ok_or_else(|| anyhow::anyhow!("unsupported symbol kind"))?,
                ),
                None => None,
            };
            serde_json::to_value(engine.search_filtered(query, kind, limit)?)?
        }
        "codegraph_explore" => {
            let query = required_string(arguments, "query")?;
            let max_files = number_argument(arguments, "maxFiles", 12, 100)?;
            serde_json::to_value(engine.explore(query, max_files)?)?
        }
        "codegraph_callers" => {
            let symbol = required_string(arguments, "symbol")?;
            let file = arguments.get("file").and_then(Value::as_str);
            let limit = number_argument(arguments, "limit", 20, 100)?;
            serde_json::to_value(engine.callers_named(symbol, file, limit)?)?
        }
        "codegraph_callees" => {
            let symbol = required_string(arguments, "symbol")?;
            let file = arguments.get("file").and_then(Value::as_str);
            let limit = number_argument(arguments, "limit", 20, 100)?;
            serde_json::to_value(engine.callees_named(symbol, file, limit)?)?
        }
        "codegraph_impact" => {
            let symbol = required_string(arguments, "symbol")?;
            let file = arguments.get("file").and_then(Value::as_str);
            let depth = number_argument(arguments, "depth", 2, 20)?;
            serde_json::to_value(engine.impact_named(symbol, file, depth)?)?
        }
        "codegraph_status" => serde_json::to_value(engine.status()?)?,
        "codegraph_files" => {
            let path = arguments.get("path").and_then(Value::as_str);
            let pattern = arguments.get("pattern").and_then(Value::as_str);
            let limit = number_argument(arguments, "limit", 1_000, 1_000)?;
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
                .take(limit)
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

fn required_string<'a>(arguments: &'a Value, name: &str) -> Result<&'a str> {
    let value = arguments
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("`{name}` must be a string"))?;
    ensure!(!value.trim().is_empty(), "`{name}` must not be empty");
    Ok(value)
}

fn number_argument(arguments: &Value, name: &str, default: usize, maximum: usize) -> Result<usize> {
    let Some(value) = arguments.get(name) else {
        return Ok(default);
    };
    let Some(value) = value.as_u64() else {
        bail!("`{name}` must be a positive integer");
    };
    ensure!(
        (1..=maximum as u64).contains(&value),
        "`{name}` must be between 1 and {maximum}"
    );
    Ok(value as usize)
}

fn parse_symbol_kind(value: &str) -> Option<crate::model::SymbolKind> {
    use crate::model::SymbolKind;
    match value {
        "function" => Some(SymbolKind::Function),
        "method" => Some(SymbolKind::Method),
        "class" => Some(SymbolKind::Class),
        "interface" => Some(SymbolKind::Interface),
        "variable" => Some(SymbolKind::Variable),
        "struct" => Some(SymbolKind::Struct),
        "trait" => Some(SymbolKind::Trait),
        "enum" => Some(SymbolKind::Enum),
        "type" => Some(SymbolKind::Type),
        "route" => Some(SymbolKind::Route),
        "component" => Some(SymbolKind::Component),
        _ => None,
    }
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
        let (mut engine, _) = Engine::init(temp.path()).unwrap();

        let names: Vec<_> = tool_definitions()
            .into_iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .collect();
        for expected in [
            "codegraph_status",
            "codegraph_files",
            "codegraph_node",
            "codegraph_explore",
            "codegraph_impact",
        ] {
            assert!(names.contains(&expected.to_owned()));
        }

        let callees = call_tool(
            &mut engine,
            &json!({ "name": "codegraph_callees", "arguments": { "symbol": "caller" } }),
        )
        .unwrap();
        assert_eq!(callees["structuredContent"][0]["symbol"]["name"], "callee");

        let explored = call_tool(
            &mut engine,
            &json!({ "name": "codegraph_explore", "arguments": { "query": "callee" } }),
        )
        .unwrap();
        assert!(explored["structuredContent"][0]["source"]
            .as_str()
            .unwrap()
            .contains("function callee"));
    }

    #[test]
    fn tool_calls_sync_changes_and_honor_project_path() {
        let default = tempfile::tempdir().unwrap();
        fs::write(default.path().join("default.ts"), "function before() {}\n").unwrap();
        let (mut engine, _) = Engine::init(default.path()).unwrap();
        fs::write(default.path().join("default.ts"), "function after() {}\n").unwrap();

        let updated = call_tool(
            &mut engine,
            &json!({ "name": "codegraph_search", "arguments": { "query": "after" } }),
        )
        .unwrap();
        assert_eq!(updated["structuredContent"][0]["symbol"]["name"], "after");

        let other = tempfile::tempdir().unwrap();
        fs::write(
            other.path().join("other.py"),
            "def elsewhere():\n    pass\n",
        )
        .unwrap();
        Engine::init(other.path()).unwrap();
        let cross_project = call_tool(
            &mut engine,
            &json!({
                "name": "codegraph_search",
                "arguments": {
                    "query": "elsewhere",
                    "projectPath": other.path()
                }
            }),
        )
        .unwrap();
        assert_eq!(
            cross_project["structuredContent"][0]["symbol"]["name"],
            "elsewhere"
        );
    }

    #[test]
    fn failed_tool_call_returns_mcp_error_content_without_protocol_failure() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("main.ts"), "function main() {}\n").unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let request = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "codegraph_search",
                "arguments": {
                    "query": "main",
                    "projectPath": temp.path().join("missing")
                }
            }
        });
        let response = handle(&mut engine, request).unwrap().unwrap();
        assert_eq!(response["id"], 7);
        assert_eq!(response["result"]["isError"], true);
        assert!(
            json_rpc_error(Value::Null, -32700, "bad")["error"]["message"]
                .as_str()
                .unwrap()
                .contains("bad")
        );
    }

    #[test]
    fn malformed_json_rpc_requests_get_standard_errors() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("main.ts"), "function main() {}\n").unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();

        let invalid_request = handle(
            &mut engine,
            json!({ "jsonrpc": "1.0", "id": 8, "method": "ping" }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(invalid_request["error"]["code"], -32600);
        assert_eq!(invalid_request["id"], 8);

        let invalid_params = handle(
            &mut engine,
            json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": [] }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(invalid_params["error"]["code"], -32602);
        assert_eq!(invalid_params["id"], 9);
    }

    #[test]
    fn tool_contract_rejects_missing_arguments_and_unbounded_limits() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("main.ts"), "function main() {}\n").unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();

        for arguments in [json!({}), json!({ "query": "main", "limit": 101 })] {
            let response = handle(
                &mut engine,
                json!({
                    "jsonrpc": "2.0",
                    "id": 10,
                    "method": "tools/call",
                    "params": {
                        "name": "codegraph_search",
                        "arguments": arguments
                    }
                }),
            )
            .unwrap()
            .unwrap();
            assert_eq!(response["result"]["isError"], true);
        }

        let schema = tool_definitions()
            .into_iter()
            .find(|tool| tool["name"] == "codegraph_search")
            .unwrap();
        assert_eq!(schema["inputSchema"]["properties"]["limit"]["maximum"], 100);
        assert_eq!(
            schema["inputSchema"]["properties"]["limit"]["type"],
            "integer"
        );
    }

    #[test]
    fn upstream_compatible_tool_listing_defaults_to_explore_only() {
        let defaults = enabled_tool_definitions(None);
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0]["name"], "codegraph_explore");

        let selected = enabled_tool_definitions(Some("explore,node,codegraph_search"));
        let names = selected
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            ["codegraph_search", "codegraph_explore", "codegraph_node"]
        );
    }

    #[test]
    fn pinned_upstream_contract_fields_remain_supported() {
        let contract: Value = serde_json::from_str(include_str!(
            "../fixtures/codegraph-1.5.0-mcp-contract.json"
        ))
        .unwrap();
        let definitions = tool_definitions();
        for (name, expected) in contract["tools"].as_object().unwrap() {
            let definition = definitions
                .iter()
                .find(|tool| tool["name"] == *name)
                .unwrap_or_else(|| panic!("missing upstream tool {name}"));
            let properties = definition["inputSchema"]["properties"].as_object().unwrap();
            for property in expected["properties"].as_array().unwrap() {
                assert!(
                    properties.contains_key(property.as_str().unwrap()),
                    "{name} is missing upstream property {property}"
                );
            }
            let required = definition["inputSchema"]["required"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                required,
                *expected["required"].as_array().unwrap(),
                "{name}"
            );
        }
        let defaults = enabled_tool_definitions(None);
        assert_eq!(
            defaults
                .iter()
                .map(|tool| tool["name"].clone())
                .collect::<Vec<_>>(),
            *contract["defaultTools"].as_array().unwrap()
        );
    }
}
