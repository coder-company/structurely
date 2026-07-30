use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use structurely::{budget::ResourceBudget, mcp, Engine};

#[derive(Parser)]
#[command(name = "structurely", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Index a project, start freshness, and connect a coding agent.
    Setup {
        /// Agent name: codex, claude, or cursor.
        client: String,
        /// Project root to prepare.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Replace the existing CodeGraph MCP entry in place.
        #[arg(long)]
        replace_codegraph: bool,
    },
    /// Create or rebuild the project index.
    Init {
        /// Project root to index.
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Index files that changed since the last graph epoch.
    Sync {
        /// Initialized project root.
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Show index freshness, size, and storage health.
    Status {
        /// Initialized project root.
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Find symbols by name or natural-language terms.
    Search {
        /// Symbol name or search terms.
        #[arg(value_parser = parse_query)]
        query: String,
        /// Initialized project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Maximum number of results (1-100).
        #[arg(long, default_value_t = 20, value_parser = parse_result_limit)]
        limit: usize,
    },
    /// Build bounded, source-backed context for a coding task.
    Explore {
        /// Task, symbol name, or search terms.
        #[arg(value_parser = parse_query)]
        query: String,
        /// Initialized project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Maximum number of starting symbols (1-100).
        #[arg(long, default_value_t = 20, value_parser = parse_result_limit)]
        limit: usize,
    },
    /// Combine semantic graph context with repository-wide content.
    Research {
        /// Task, question, or search terms.
        #[arg(value_parser = parse_query)]
        query: String,
        /// Initialized project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Maximum number of distinct files (1-100).
        #[arg(long, default_value_t = 12, value_parser = parse_result_limit)]
        max_files: usize,
    },
    /// List symbols that call a named symbol.
    Callers {
        /// Exact symbol name.
        #[arg(value_parser = parse_identifier)]
        symbol: String,
        /// Optional project-relative file used to disambiguate the symbol.
        #[arg(long, value_parser = parse_identifier)]
        file: Option<String>,
        /// Initialized project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Maximum number of results (1-100).
        #[arg(long, default_value_t = 20, value_parser = parse_result_limit)]
        limit: usize,
    },
    /// List symbols called by a named symbol.
    Callees {
        /// Exact symbol name.
        #[arg(value_parser = parse_identifier)]
        symbol: String,
        /// Optional project-relative file used to disambiguate the symbol.
        #[arg(long, value_parser = parse_identifier)]
        file: Option<String>,
        /// Initialized project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Maximum number of results (1-100).
        #[arg(long, default_value_t = 20, value_parser = parse_result_limit)]
        limit: usize,
    },
    /// Traverse callers to estimate the impact of changing a symbol.
    Impact {
        /// Exact symbol name.
        #[arg(value_parser = parse_identifier)]
        symbol: String,
        /// Optional project-relative file used to disambiguate the symbol.
        #[arg(long, value_parser = parse_identifier)]
        file: Option<String>,
        /// Initialized project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Maximum traversal depth (1-20).
        #[arg(long, default_value_t = 2, value_parser = parse_traversal_depth)]
        depth: usize,
    },
    /// Trace the shortest evidence-backed path between two symbols.
    Trace {
        /// Starting symbol name, qualified name, or stable ID.
        #[arg(value_parser = parse_identifier)]
        source: String,
        /// Target symbol name, qualified name, or stable ID.
        #[arg(value_parser = parse_identifier)]
        target: String,
        /// Optional source file suffix used to disambiguate the symbol.
        #[arg(long, value_parser = parse_identifier)]
        source_file: Option<String>,
        /// Optional target file suffix used to disambiguate the symbol.
        #[arg(long, value_parser = parse_identifier)]
        target_file: Option<String>,
        /// Initialized project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Maximum traversal depth (1-20).
        #[arg(long, default_value_t = 6, value_parser = parse_traversal_depth)]
        depth: usize,
    },
    /// Manage durable team workspace namespaces.
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    /// Record and inspect durable coding-agent sessions.
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Generate a deterministic recap from a session's event history.
    Recap {
        /// Session ID.
        session: String,
        /// Project root containing durable state.
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
    /// Store and retrieve durable workspace memory.
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
    },
    /// Back up or restore authoritative durable state.
    State {
        #[command(subcommand)]
        command: StateCommand,
    },
    /// Export a deterministic JSON snapshot of the graph.
    Snapshot {
        /// Initialized project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
    /// Measure indexing and query latency.
    Benchmark {
        /// Project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Search query used for the query benchmark.
        #[arg(long, default_value = "main", value_parser = parse_query)]
        query: String,
        /// Query iterations (1-1000).
        #[arg(long, default_value_t = 20, value_parser = parse_benchmark_iterations)]
        iterations: usize,
    },
    /// Compare the graph with an expected semantic manifest.
    Quality {
        /// Initialized project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Path to the quality manifest.
        #[arg(long, default_value = "quality.json")]
        manifest: PathBuf,
    },
    /// Watch a project and publish incremental graph epochs.
    Watch {
        /// Initialized project root.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Filesystem event debounce in milliseconds.
        #[arg(long, default_value_t = 250)]
        debounce_ms: u64,
    },
    /// Manage the background project indexer.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Configure a coding agent to use Structurely over MCP.
    Integrations {
        #[command(subcommand)]
        command: IntegrationCommand,
    },
    /// Run a protocol server.
    Serve {
        /// Serve newline-delimited MCP over standard input and output.
        #[arg(long)]
        mcp: bool,
        /// Initialized project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
}

fn parse_query(value: &str) -> std::result::Result<String, String> {
    ResourceBudget::query(value)
        .map(str::to_owned)
        .map_err(|error| error.to_string())
}

fn parse_identifier(value: &str) -> std::result::Result<String, String> {
    ResourceBudget::identifier(value)
        .map(str::to_owned)
        .map_err(|error| error.to_string())
}

fn parse_result_limit(value: &str) -> std::result::Result<usize, String> {
    let value = value
        .parse::<usize>()
        .map_err(|_| "result limit must be a positive integer".to_owned())?;
    ResourceBudget::result_limit(value).map_err(|error| error.to_string())
}

fn parse_traversal_depth(value: &str) -> std::result::Result<usize, String> {
    let value = value
        .parse::<usize>()
        .map_err(|_| "traversal depth must be a positive integer".to_owned())?;
    ResourceBudget::traversal_depth(value).map_err(|error| error.to_string())
}

fn parse_benchmark_iterations(value: &str) -> std::result::Result<usize, String> {
    let value = value
        .parse::<usize>()
        .map_err(|_| "benchmark iterations must be a positive integer".to_owned())?;
    ResourceBudget::benchmark_iterations(value).map_err(|error| error.to_string())
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Start the background indexer.
    Start {
        /// Initialized project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Filesystem event debounce in milliseconds.
        #[arg(long, default_value_t = 250)]
        debounce_ms: u64,
    },
    /// Show background indexer health and freshness.
    Status {
        /// Initialized project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
    /// Stop the background indexer.
    Stop {
        /// Initialized project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
    #[command(hide = true)]
    Run {
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 250)]
        debounce_ms: u64,
    },
}

#[derive(Subcommand)]
enum IntegrationCommand {
    /// Add Structurely to a project-local agent configuration.
    Install {
        /// Agent name: codex, claude, or cursor.
        client: String,
        /// Project root whose agent configuration to update.
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
    /// Check a project-local agent configuration.
    Status {
        /// Agent name: codex, claude, or cursor.
        client: String,
        /// Project root whose agent configuration to inspect.
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
    /// Remove only Structurely's project-local agent entry.
    Uninstall {
        /// Agent name: codex, claude, or cursor.
        client: String,
        /// Project root whose agent configuration to update.
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum WorkspaceCommand {
    /// Create a workspace.
    Create {
        name: String,
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
    /// List workspaces.
    List {
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 20, value_parser = parse_result_limit)]
        limit: usize,
    },
    /// Rename a workspace.
    Rename {
        id: String,
        name: String,
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum SessionCommand {
    /// Start a session in a workspace.
    Start {
        workspace: String,
        title: String,
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
    /// List session history.
    List {
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 20, value_parser = parse_result_limit)]
        limit: usize,
    },
    /// Append an event to an active session.
    Add {
        session: String,
        kind: String,
        body: String,
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
    /// Show a session and its ordered event history.
    Show {
        session: String,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 100, value_parser = parse_result_limit)]
        limit: usize,
    },
    /// Complete a session.
    End {
        session: String,
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum MemoryCommand {
    /// Save durable memory in a workspace.
    Remember {
        workspace: String,
        body: String,
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
    /// Search durable memory in a workspace.
    Recall {
        workspace: String,
        query: String,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 10, value_parser = parse_result_limit)]
        limit: usize,
    },
    /// Remove one memory by ID.
    Forget {
        id: String,
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum StateCommand {
    /// Create a consistent standalone state database snapshot.
    Backup {
        /// Destination snapshot file.
        destination: PathBuf,
        /// Project root containing durable state.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Replace an existing destination file.
        #[arg(long)]
        force: bool,
    },
    /// Validate and atomically replace live state from a snapshot.
    Restore {
        /// Source snapshot file.
        source: PathBuf,
        /// Project root whose durable state will be replaced.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Confirm replacement of the live state database.
        #[arg(long)]
        force: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Setup {
            client,
            path,
            replace_codegraph,
        } => {
            let client = structurely::integrations::AgentClient::parse(&client)?;
            let executable = std::env::current_exe()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&structurely::setup::run(
                    path,
                    client,
                    executable,
                    replace_codegraph
                )?)?
            );
        }
        Command::Init { path } => {
            let (_, report) = Engine::init(path)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Sync { path } => {
            let mut engine = Engine::open(path)?;
            println!("{}", serde_json::to_string_pretty(&engine.sync()?)?);
        }
        Command::Status { path } => {
            let engine = Engine::open_read_only(path)?;
            println!("{}", serde_json::to_string_pretty(&engine.status()?)?);
        }
        Command::Search { query, path, limit } => {
            let engine = Engine::open_read_only(path)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&engine.search(&query, limit)?)?
            );
        }
        Command::Explore { query, path, limit } => {
            let engine = Engine::open_read_only(path)?;
            let hits = engine.explore(&query, limit)?;
            print!("{}", mcp::format_explore_text(&engine, &query, &hits)?);
        }
        Command::Research {
            query,
            path,
            max_files,
        } => {
            let engine = Engine::open_read_only(path)?;
            let report =
                structurely::workflow::WorkflowService::new(&engine).research(&query, max_files)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Callers {
            symbol,
            file,
            path,
            limit,
        } => {
            let engine = Engine::open_read_only(path)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&engine.callers_named(
                    &symbol,
                    file.as_deref(),
                    limit
                )?)?
            );
        }
        Command::Callees {
            symbol,
            file,
            path,
            limit,
        } => {
            let engine = Engine::open_read_only(path)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&engine.callees_named(
                    &symbol,
                    file.as_deref(),
                    limit
                )?)?
            );
        }
        Command::Impact {
            symbol,
            file,
            path,
            depth,
        } => {
            let engine = Engine::open_read_only(path)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&engine.impact_named(
                    &symbol,
                    file.as_deref(),
                    depth
                )?)?
            );
        }
        Command::Trace {
            source,
            target,
            source_file,
            target_file,
            path,
            depth,
        } => {
            let engine = Engine::open_read_only(path)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&engine.trace_path_named(
                    &source,
                    source_file.as_deref(),
                    &target,
                    target_file.as_deref(),
                    depth,
                )?)?
            );
        }
        Command::Workspace { command } => match command {
            WorkspaceCommand::Create { name, path } => {
                let store = structurely::state::StateStore::open(path)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&store.create_workspace(&name)?)?
                );
            }
            WorkspaceCommand::List { path, limit } => {
                let store = structurely::state::StateStore::open(path)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&store.list_workspaces(limit)?)?
                );
            }
            WorkspaceCommand::Rename { id, name, path } => {
                let store = structurely::state::StateStore::open(path)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&store.rename_workspace(&id, &name)?)?
                );
            }
        },
        Command::Session { command } => match command {
            SessionCommand::Start {
                workspace,
                title,
                path,
            } => {
                let store = structurely::state::StateStore::open(path)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&store.create_session(&workspace, &title)?)?
                );
            }
            SessionCommand::List {
                workspace,
                path,
                limit,
            } => {
                let store = structurely::state::StateStore::open(path)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &store.list_sessions(workspace.as_deref(), limit)?
                    )?
                );
            }
            SessionCommand::Add {
                session,
                kind,
                body,
                path,
            } => {
                let mut store = structurely::state::StateStore::open(path)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&store.append_event(&session, &kind, &body)?)?
                );
            }
            SessionCommand::Show {
                session,
                path,
                limit,
            } => {
                let store = structurely::state::StateStore::open(path)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "session": store.session(&session)?,
                        "events": store.events(&session, limit)?,
                        "recap": store.recap(&session)?,
                    }))?
                );
            }
            SessionCommand::End { session, path } => {
                let store = structurely::state::StateStore::open(path)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&store.complete_session(&session)?)?
                );
            }
        },
        Command::Recap { session, path } => {
            let store = structurely::state::StateStore::open(path)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&store.generate_recap(&session)?)?
            );
        }
        Command::Memory { command } => match command {
            MemoryCommand::Remember {
                workspace,
                body,
                tags,
                path,
            } => {
                let mut store = structurely::state::StateStore::open(path)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&store.remember(&workspace, &body, &tags)?)?
                );
            }
            MemoryCommand::Recall {
                workspace,
                query,
                path,
                limit,
            } => {
                let store = structurely::state::StateStore::open(path)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &store.search_memories(&workspace, &query, limit)?
                    )?
                );
            }
            MemoryCommand::Forget { id, path } => {
                let mut store = structurely::state::StateStore::open(path)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "id": id,
                        "forgotten": store.forget(&id)?,
                    }))?
                );
            }
        },
        Command::State { command } => match command {
            StateCommand::Backup {
                destination,
                path,
                force,
            } => {
                let store = structurely::state::StateStore::open(path)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&store.backup(destination, force)?)?
                );
            }
            StateCommand::Restore {
                source,
                path,
                force,
            } => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&structurely::state::StateStore::restore(
                        path, source, force
                    )?)?
                );
            }
        },
        Command::Snapshot { path } => {
            let engine = Engine::open_read_only(path)?;
            println!("{}", serde_json::to_string_pretty(&engine.snapshot()?)?);
        }
        Command::Benchmark {
            path,
            query,
            iterations,
        } => {
            let (mut engine, initial) = if path.join(structurely::engine::PROJECT_DIR).exists() {
                let mut engine = Engine::open(&path)?;
                let initial = engine.sync()?;
                (engine, initial)
            } else {
                Engine::init(&path)?
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&engine.benchmark(&query, iterations, initial)?)?
            );
        }
        Command::Quality { path, manifest } => {
            let engine = Engine::open_read_only(path)?;
            let report = engine.evaluate_quality(manifest)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.passed {
                anyhow::bail!("semantic quality manifest did not match the indexed graph");
            }
        }
        Command::Watch { path, debounce_ms } => {
            let mut engine = Engine::open(path)?;
            let stop = Arc::new(AtomicBool::new(false));
            let signal = Arc::clone(&stop);
            ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
            eprintln!(
                "Watching {} (debounce: {} ms). Press Ctrl-C to stop.",
                engine.root().display(),
                debounce_ms
            );
            engine.watch(stop, Duration::from_millis(debounce_ms.max(10)), |report| {
                if let Ok(rendered) = serde_json::to_string(report) {
                    println!("{rendered}");
                }
            })?;
        }
        Command::Daemon {
            command: DaemonCommand::Start { path, debounce_ms },
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&structurely::daemon::start(
                    path,
                    Duration::from_millis(debounce_ms.max(10))
                )?)?
            );
        }
        Command::Daemon {
            command: DaemonCommand::Status { path },
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&structurely::daemon::status(path)?)?
            );
        }
        Command::Daemon {
            command: DaemonCommand::Stop { path },
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&structurely::daemon::stop(path)?)?
            );
        }
        Command::Daemon {
            command: DaemonCommand::Run { path, debounce_ms },
        } => {
            structurely::daemon::run(path, Duration::from_millis(debounce_ms.max(10)))?;
        }
        Command::Integrations {
            command: IntegrationCommand::Install { client, path },
        } => {
            let client = structurely::integrations::AgentClient::parse(&client)?;
            let executable = std::env::current_exe()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&structurely::integrations::install(
                    path, client, executable
                )?)?
            );
        }
        Command::Integrations {
            command: IntegrationCommand::Status { client, path },
        } => {
            let client = structurely::integrations::AgentClient::parse(&client)?;
            let executable = std::env::current_exe()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&structurely::integrations::status(
                    path, client, executable
                )?)?
            );
        }
        Command::Integrations {
            command: IntegrationCommand::Uninstall { client, path },
        } => {
            let client = structurely::integrations::AgentClient::parse(&client)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&structurely::integrations::uninstall(path, client)?)?
            );
        }
        Command::Serve { mcp: true, path } => mcp::serve_stdio(&path)?,
        Command::Serve { mcp: false, .. } => {
            anyhow::bail!("only `structurely serve --mcp` is currently supported")
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_rejects_resource_budget_overflow_before_opening_a_project() {
        assert!(Cli::try_parse_from([
            "structurely",
            "search",
            "main",
            "--limit",
            &(ResourceBudget::MAX_RESULTS + 1).to_string(),
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "structurely",
            "impact",
            "main",
            "--depth",
            &(ResourceBudget::MAX_TRAVERSAL_DEPTH + 1).to_string(),
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "structurely",
            "search",
            &"q".repeat(ResourceBudget::MAX_QUERY_BYTES + 1),
        ])
        .is_err());
    }
}
