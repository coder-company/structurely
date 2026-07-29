use crate::{budget::ResourceBudget, daemon, Engine};
use anyhow::{bail, ensure, Result};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

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
            "protocolVersion": negotiated_protocol_version(&request),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "structurely", "version": env!("CARGO_PKG_VERSION") },
            "instructions": "Use codegraph_explore before reading files when you need task-oriented code context. Treat returned source locations as authoritative, and check freshness metadata before doing a manual repository search."
        }),
        "tools/list" => json!({ "tools": enabled_tool_definitions(
            std::env::var("CODEGRAPH_MCP_TOOLS").ok().as_deref()
        ) }),
        "resources/list" => json!({ "resources": [] }),
        "resources/templates/list" => json!({ "resourceTemplates": [] }),
        "prompts/list" => json!({ "prompts": [] }),
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

fn negotiated_protocol_version(request: &Value) -> &'static str {
    match request["params"]["protocolVersion"].as_str() {
        Some("2025-06-18") => "2025-06-18",
        Some("2025-03-26") => "2025-03-26",
        Some("2024-11-05") => "2024-11-05",
        _ => "2024-11-05",
    }
}

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "codegraph_search",
            "description": "Quick symbol search by name. Returns locations only.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": ResourceBudget::MAX_QUERY_BYTES
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["function", "method", "class", "interface", "type", "variable", "route", "component"]
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": ResourceBudget::MAX_RESULTS,
                        "default": 10
                    },
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
                    "query": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": ResourceBudget::MAX_QUERY_BYTES
                    },
                    "maxFiles": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": ResourceBudget::MAX_RESULTS,
                        "default": 12
                    },
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
                    "symbol": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": ResourceBudget::MAX_IDENTIFIER_BYTES
                    },
                    "file": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": ResourceBudget::MAX_IDENTIFIER_BYTES
                    },
                    "depth": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": ResourceBudget::MAX_TRAVERSAL_DEPTH,
                        "default": 2
                    },
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
                    "symbol": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": ResourceBudget::MAX_IDENTIFIER_BYTES
                    },
                    "includeCode": { "type": "boolean", "default": false },
                    "file": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": ResourceBudget::MAX_IDENTIFIER_BYTES
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": ResourceBudget::MAX_NODE_OFFSET
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": ResourceBudget::MAX_NODE_LINES
                    },
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
                "symbol": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": ResourceBudget::MAX_IDENTIFIER_BYTES
                },
                "file": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": ResourceBudget::MAX_IDENTIFIER_BYTES
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": ResourceBudget::MAX_RESULTS,
                    "default": 20
                },
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
        return dispatch_freshness_aware(&mut project_engine, name, &arguments);
    }
    dispatch_freshness_aware(engine, name, &arguments)
}

fn dispatch_freshness_aware(engine: &mut Engine, name: &str, arguments: &Value) -> Result<Value> {
    let refresh = refresh_index(engine);
    let mut response = dispatch_tool(engine, name, arguments)?;
    let freshness = match &refresh.error {
        Some(error) => json!({
            "state": "stale",
            "mode": refresh.mode,
            "epoch": refresh.epoch,
            "warning": format!(
                "Index catch-up failed; results use the last committed graph epoch: {error}"
            )
        }),
        None => json!({
            "state": "current",
            "mode": refresh.mode,
            "epoch": refresh.epoch,
            "daemonPid": refresh.daemon_pid
        }),
    };
    response["_meta"] = json!({ "freshness": freshness });
    if let Some(error) = refresh.error {
        let warning = format!(
            "⚠ **Stale index:** catch-up failed, so this result uses the last committed graph epoch. \
             Cause: {error}\n\n"
        );
        if let Some(text) = response
            .get_mut("content")
            .and_then(Value::as_array_mut)
            .and_then(|content| content.first_mut())
            .and_then(|item| item.get_mut("text"))
            .and_then(|value| value.as_str())
        {
            let decorated = format!("{warning}{text}");
            response["content"][0]["text"] = Value::String(decorated);
        }
    }
    Ok(response)
}

struct RefreshOutcome {
    mode: &'static str,
    epoch: Option<u64>,
    daemon_pid: Option<u32>,
    error: Option<anyhow::Error>,
}

fn refresh_index(engine: &mut Engine) -> RefreshOutcome {
    const DAEMON_CATCH_UP: Duration = Duration::from_millis(500);
    match daemon::status(engine.root()) {
        Ok(status) if status.running => {
            let daemon_pid = status.state.as_ref().map(|state| state.pid);
            let deadline = Instant::now() + DAEMON_CATCH_UP;
            loop {
                match engine.status() {
                    Ok(project) if project.pending_files == 0 => {
                        return RefreshOutcome {
                            mode: "daemon",
                            epoch: Some(project.epoch),
                            daemon_pid,
                            error: None,
                        };
                    }
                    Ok(_) if Instant::now() < deadline => {
                        thread::sleep(Duration::from_millis(25));
                    }
                    Ok(_) | Err(_) => break,
                }
            }
            match engine.sync() {
                Ok(report) => RefreshOutcome {
                    mode: "foreground-fallback",
                    epoch: Some(report.epoch),
                    daemon_pid,
                    error: None,
                },
                Err(error) => RefreshOutcome {
                    mode: "foreground-fallback",
                    epoch: engine.status().ok().map(|project| project.epoch),
                    daemon_pid,
                    error: Some(error),
                },
            }
        }
        Ok(_) => foreground_refresh(engine, "foreground"),
        Err(_) => foreground_refresh(engine, "foreground-fallback"),
    }
}

fn foreground_refresh(engine: &mut Engine, mode: &'static str) -> RefreshOutcome {
    match engine.sync() {
        Ok(report) => RefreshOutcome {
            mode,
            epoch: Some(report.epoch),
            daemon_pid: None,
            error: None,
        },
        Err(error) => RefreshOutcome {
            mode,
            epoch: engine.status().ok().map(|project| project.epoch),
            daemon_pid: None,
            error: Some(error),
        },
    }
}

fn dispatch_tool(engine: &Engine, name: &str, arguments: &Value) -> Result<Value> {
    let mut text_override = None;
    let payload = match name {
        "codegraph_search" => {
            let query = required_string(arguments, "query")?;
            let limit = number_argument(arguments, "limit", 10, ResourceBudget::MAX_RESULTS)?;
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
            let max_files =
                number_argument(arguments, "maxFiles", 12, ResourceBudget::MAX_RESULTS)?;
            let hits = engine.explore(query, max_files)?;
            text_override = Some(format_explore_text(engine, query, &hits)?);
            serde_json::to_value(hits)?
        }
        "codegraph_callers" => {
            let symbol = required_string(arguments, "symbol")?;
            let file = optional_string(arguments, "file")?;
            let limit = number_argument(arguments, "limit", 20, ResourceBudget::MAX_RESULTS)?;
            serde_json::to_value(engine.callers_named(symbol, file, limit)?)?
        }
        "codegraph_callees" => {
            let symbol = required_string(arguments, "symbol")?;
            let file = optional_string(arguments, "file")?;
            let limit = number_argument(arguments, "limit", 20, ResourceBudget::MAX_RESULTS)?;
            serde_json::to_value(engine.callees_named(symbol, file, limit)?)?
        }
        "codegraph_impact" => {
            let symbol = required_string(arguments, "symbol")?;
            let file = optional_string(arguments, "file")?;
            let depth =
                number_argument(arguments, "depth", 2, ResourceBudget::MAX_TRAVERSAL_DEPTH)?;
            serde_json::to_value(engine.impact_named(symbol, file, depth)?)?
        }
        "codegraph_status" => serde_json::to_value(engine.status()?)?,
        "codegraph_files" => {
            let path = optional_string(arguments, "path")?;
            let pattern = optional_string(arguments, "pattern")?;
            let limit = number_argument(arguments, "limit", 1_000, 1_000)?;
            let matcher = pattern
                .map(globset::Glob::new)
                .transpose()?
                .map(|glob| glob.compile_matcher());
            let matching_files = engine
                .files()?
                .into_iter()
                .filter(|file| path.is_none_or(|prefix| file.path.starts_with(prefix)))
                .filter(|file| {
                    matcher
                        .as_ref()
                        .is_none_or(|glob| glob.is_match(&file.path))
                })
                .collect::<Vec<_>>();
            let omitted = matching_files.len().saturating_sub(limit);
            let files = matching_files.into_iter().take(limit).collect::<Vec<_>>();
            if omitted > 0 {
                text_override = Some(format!(
                    "{}\n\nShowing {} files; {omitted} omitted by the {limit}-file limit.",
                    serde_json::to_string_pretty(&files)?,
                    files.len()
                ));
            }
            serde_json::to_value(files)?
        }
        "codegraph_node" => {
            let symbol = optional_string(arguments, "symbol")?;
            let file = optional_string(arguments, "file")?;
            let include_code = optional_bool_argument(arguments, "includeCode")?.unwrap_or(false);
            let offset =
                optional_number_argument(arguments, "offset", 0, ResourceBudget::MAX_NODE_OFFSET)?;
            let limit =
                optional_number_argument(arguments, "limit", 1, ResourceBudget::MAX_NODE_LINES)?;
            let symbols_only = optional_bool_argument(arguments, "symbolsOnly")?.unwrap_or(false);
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
    let text = match text_override {
        Some(text) => text,
        None => serde_json::to_string_pretty(&payload)?,
    };
    Ok(json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": payload,
        "isError": false
    }))
}

pub fn format_explore_text(
    engine: &Engine,
    query: &str,
    hits: &[crate::engine::ExploreHit],
) -> Result<String> {
    use std::fmt::Write as _;

    let indexed_files = engine.files()?.len();
    let maximum_chars = match indexed_files {
        0..=499 => 48_000,
        500..=4_999 => 32_000,
        _ => 24_000,
    };
    let mut ranked = hits.iter().collect::<Vec<_>>();
    let normalized_query = query.trim().to_ascii_lowercase();
    ranked.sort_by(|left, right| {
        let left_exact = left.symbol.name.to_ascii_lowercase() == normalized_query;
        let right_exact = right.symbol.name.to_ascii_lowercase() == normalized_query;
        right_exact
            .cmp(&left_exact)
            .then_with(|| {
                (right.callers.len()
                    + right.callees.len()
                    + right.referenced_by.len()
                    + right.references.len())
                .cmp(
                    &(left.callers.len()
                        + left.callees.len()
                        + left.referenced_by.len()
                        + left.references.len()),
                )
            })
            .then_with(|| left.symbol.file.cmp(&right.symbol.file))
            .then_with(|| left.symbol.start_line.cmp(&right.symbol.start_line))
    });
    let total_files = hits
        .iter()
        .map(|hit| hit.symbol.file.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();
    let mut output = format!(
        "**Exploration: {query}**\n\nFound {} symbols across {} files.\n",
        hits.len(),
        total_files
    );
    output.push_str(
        "\n**Source Code**\n\n\
         > Ranked current-source excerpts, line-numbered and globally budgeted.\n",
    );
    let mut rendered_symbols = 0;
    let mut rendered_files = std::collections::HashSet::new();
    const FOOTER_RESERVE: usize = 600;
    for hit in ranked {
        let mut section = String::new();
        writeln!(
            section,
            "\n**`{}` — `{}` ({}, lines {}–{})**\n",
            hit.symbol.file,
            hit.symbol.qualified_name,
            hit.symbol.kind,
            hit.symbol.start_line,
            hit.symbol.end_line
        )?;
        if !hit.callers.is_empty()
            || !hit.callees.is_empty()
            || !hit.referenced_by.is_empty()
            || !hit.references.is_empty()
        {
            let callers = hit
                .callers
                .iter()
                .take(8)
                .map(|(symbol, _)| symbol.qualified_name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let callees = hit
                .callees
                .iter()
                .take(8)
                .map(|(symbol, _)| symbol.qualified_name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(
                section,
                "- Flow: {} caller{}, {} callee{}{}{}",
                hit.callers.len(),
                if hit.callers.len() == 1 { "" } else { "s" },
                hit.callees.len(),
                if hit.callees.len() == 1 { "" } else { "s" },
                if callers.is_empty() {
                    ""
                } else {
                    " — callers: "
                },
                callers
            )?;
            if !callees.is_empty() {
                writeln!(section, "- Callees: {callees}")?;
            }
            let referenced_by = hit
                .referenced_by
                .iter()
                .take(8)
                .map(|(symbol, _)| symbol.qualified_name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let references = hit
                .references
                .iter()
                .take(8)
                .map(|(symbol, _)| symbol.qualified_name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            if !referenced_by.is_empty() {
                writeln!(section, "- Referenced by: {referenced_by}")?;
            }
            if !references.is_empty() {
                writeln!(section, "- References: {references}")?;
            }
        }
        if hit.relationships_truncated {
            writeln!(
                section,
                "- Flow details capped at {} relationships per direction.",
                ResourceBudget::MAX_EXPLORE_RELATIONSHIPS
            )?;
        }
        writeln!(section, "\n```{}", hit.symbol.language)?;
        for (offset, line) in hit.source.lines().enumerate() {
            writeln!(section, "{}\t{}", hit.symbol.start_line + offset, line)?;
        }
        if hit.source_truncated {
            writeln!(section, "… source excerpt truncated at 4,000 characters")?;
        }
        section.push_str("```\n");
        if output.chars().count() + section.chars().count() + FOOTER_RESERVE > maximum_chars {
            continue;
        }
        rendered_files.insert(hit.symbol.file.as_str());
        rendered_symbols += 1;
        output.push_str(&section);
    }
    let omitted_symbols = hits.len().saturating_sub(rendered_symbols);
    writeln!(
        output,
        "\n**Coverage:** rendered {rendered_symbols}/{} symbols across {}/{} files; \
         omitted {omitted_symbols} symbol{} under the {maximum_chars}-character global budget.",
        hits.len(),
        rendered_files.len(),
        total_files,
        if omitted_symbols == 1 { "" } else { "s" }
    )?;
    Ok(output)
}

fn required_string<'a>(arguments: &'a Value, name: &str) -> Result<&'a str> {
    let value = arguments
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("`{name}` must be a string"))?;
    ensure!(!value.trim().is_empty(), "`{name}` must not be empty");
    Ok(value)
}

fn optional_string<'a>(arguments: &'a Value, name: &str) -> Result<Option<&'a str>> {
    let Some(value) = arguments.get(name) else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("`{name}` must be a string"))?;
    ResourceBudget::identifier(value)?;
    Ok(Some(value))
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

fn optional_number_argument(
    arguments: &Value,
    name: &str,
    minimum: usize,
    maximum: usize,
) -> Result<Option<usize>> {
    let Some(value) = arguments.get(name) else {
        return Ok(None);
    };
    let Some(value) = value.as_u64() else {
        bail!("`{name}` must be an integer");
    };
    ensure!(
        (minimum as u64..=maximum as u64).contains(&value),
        "`{name}` must be between {minimum} and {maximum}"
    );
    Ok(Some(value as usize))
}

fn optional_bool_argument(arguments: &Value, name: &str) -> Result<Option<bool>> {
    let Some(value) = arguments.get(name) else {
        return Ok(None);
    };
    value
        .as_bool()
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("`{name}` must be a boolean"))
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
        let text = explored["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("**Exploration: callee**"));
        assert!(text.contains("**Source Code**"));
        assert!(text.contains("2\tfunction callee()"));
        assert!(text.contains("**Coverage:**"));
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
    fn tool_calls_disclose_stale_fallback_and_recover_after_sync_errors() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("main.ts"),
            "export function committedSymbol() {}\n",
        )
        .unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let invalid_config = temp.path().join("structurely.json");
        fs::write(&invalid_config, "{not-json").unwrap();

        let stale = call_tool(
            &mut engine,
            &json!({
                "name": "codegraph_search",
                "arguments": { "query": "committedSymbol" }
            }),
        )
        .unwrap();
        assert_eq!(stale["_meta"]["freshness"]["state"], "stale");
        assert!(stale["_meta"]["freshness"]["warning"]
            .as_str()
            .unwrap()
            .contains("last committed graph epoch"));
        assert!(stale["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("⚠ **Stale index:**"));
        assert_eq!(
            stale["structuredContent"][0]["symbol"]["name"],
            "committedSymbol"
        );

        fs::remove_file(invalid_config).unwrap();
        let current = call_tool(
            &mut engine,
            &json!({
                "name": "codegraph_search",
                "arguments": { "query": "committedSymbol" }
            }),
        )
        .unwrap();
        assert_eq!(current["_meta"]["freshness"]["state"], "current");
        assert_eq!(current["_meta"]["freshness"]["mode"], "foreground");
        assert!(!current["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Stale index"));
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
    fn initialize_negotiates_supported_versions_with_upstream_fallback() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("main.ts"), "function main() {}\n").unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();

        for (requested, expected) in [
            ("2024-11-05", "2024-11-05"),
            ("2025-03-26", "2025-03-26"),
            ("2025-06-18", "2025-06-18"),
            ("2099-01-01", "2024-11-05"),
        ] {
            let response = handle(
                &mut engine,
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": { "protocolVersion": requested }
                }),
            )
            .unwrap()
            .unwrap();
            assert_eq!(response["result"]["protocolVersion"], expected);
        }
    }

    #[test]
    fn unsupported_mcp_capability_probes_return_successful_empty_lists() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("main.ts"), "function main() {}\n").unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();

        for (id, method, expected) in [
            (11, "resources/list", json!({ "resources": [] })),
            (
                12,
                "resources/templates/list",
                json!({ "resourceTemplates": [] }),
            ),
            (13, "prompts/list", json!({ "prompts": [] })),
        ] {
            let response = handle(
                &mut engine,
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": method,
                    "params": {}
                }),
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                response,
                json!({ "jsonrpc": "2.0", "id": id, "result": expected })
            );
        }
    }

    #[test]
    fn tool_contract_rejects_missing_arguments_and_unbounded_limits() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("main.ts"), "function main() {}\n").unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();

        for arguments in [
            json!({}),
            json!({ "query": "main", "limit": ResourceBudget::MAX_RESULTS + 1 }),
            json!({ "query": "q".repeat(ResourceBudget::MAX_QUERY_BYTES + 1) }),
        ] {
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

        for arguments in [
            json!({ "file": 42 }),
            json!({ "file": "main.ts", "offset": ResourceBudget::MAX_NODE_OFFSET + 1 }),
            json!({ "file": "main.ts", "limit": ResourceBudget::MAX_NODE_LINES + 1 }),
            json!({ "file": "main.ts", "includeCode": "yes" }),
        ] {
            let response = handle(
                &mut engine,
                json!({
                    "jsonrpc": "2.0",
                    "id": 11,
                    "method": "tools/call",
                    "params": {
                        "name": "codegraph_node",
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
        assert_eq!(
            schema["inputSchema"]["properties"]["limit"]["maximum"],
            ResourceBudget::MAX_RESULTS
        );
        assert_eq!(
            schema["inputSchema"]["properties"]["query"]["maxLength"],
            ResourceBudget::MAX_QUERY_BYTES
        );
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

    #[test]
    fn explore_enforces_global_and_per_symbol_budgets_with_disclosure() {
        let temp = tempfile::tempdir().unwrap();
        let source = (0..24)
            .map(|index| {
                format!(
                    "function handler{index}() {{ return \"{}\"; }}\n",
                    "x".repeat(5_000)
                )
            })
            .collect::<String>();
        fs::write(temp.path().join("handlers.ts"), source).unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let mut hits = engine.explore("handler", 12).unwrap();
        assert!(hits.iter().any(|hit| hit.source_truncated));
        hits[0].relationships_truncated = true;

        let text = format_explore_text(&engine, "handler", &hits).unwrap();
        assert!(text.chars().count() <= 48_000);
        assert!(text.contains("source excerpt truncated at 4,000 characters"));
        assert!(text.contains("Flow details capped at 8 relationships per direction"));
        assert!(!text.contains("omitted 0 symbols"));
        assert!(text.contains("character global budget"));
    }

    #[test]
    fn explore_ranks_exact_symbols_before_loose_matches() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("orders.ts"),
            "function processOrderLegacy() {}\nfunction processOrder() {}\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let hits = engine.explore("processOrder", 12).unwrap();
        let text = format_explore_text(&engine, "processOrder", &hits).unwrap();
        assert!(text
            .find("`processOrder`")
            .unwrap()
            .lt(&text.find("`processOrderLegacy`").unwrap()));
    }
}
