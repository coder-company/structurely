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
    Init {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    Sync {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    Status {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    Search {
        #[arg(value_parser = parse_query)]
        query: String,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 20, value_parser = parse_result_limit)]
        limit: usize,
    },
    Explore {
        #[arg(value_parser = parse_query)]
        query: String,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 20, value_parser = parse_result_limit)]
        limit: usize,
    },
    Callers {
        #[arg(value_parser = parse_identifier)]
        symbol: String,
        #[arg(long, value_parser = parse_identifier)]
        file: Option<String>,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 20, value_parser = parse_result_limit)]
        limit: usize,
    },
    Callees {
        #[arg(value_parser = parse_identifier)]
        symbol: String,
        #[arg(long, value_parser = parse_identifier)]
        file: Option<String>,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 20, value_parser = parse_result_limit)]
        limit: usize,
    },
    Impact {
        #[arg(value_parser = parse_identifier)]
        symbol: String,
        #[arg(long, value_parser = parse_identifier)]
        file: Option<String>,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 2, value_parser = parse_traversal_depth)]
        depth: usize,
    },
    Snapshot {
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
    Benchmark {
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value = "main", value_parser = parse_query)]
        query: String,
        #[arg(long, default_value_t = 20, value_parser = parse_benchmark_iterations)]
        iterations: usize,
    },
    Quality {
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value = "quality.json")]
        manifest: PathBuf,
    },
    Watch {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 250)]
        debounce_ms: u64,
    },
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    Integrations {
        #[command(subcommand)]
        command: IntegrationCommand,
    },
    Serve {
        #[arg(long)]
        mcp: bool,
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
    Start {
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 250)]
        debounce_ms: u64,
    },
    Status {
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
    Stop {
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
    Install {
        client: String,
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
    Status {
        client: String,
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
    Uninstall {
        client: String,
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init { path } => {
            let (_, report) = Engine::init(path)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Sync { path } => {
            let mut engine = Engine::open(path)?;
            println!("{}", serde_json::to_string_pretty(&engine.sync()?)?);
        }
        Command::Status { path } => {
            let engine = Engine::open(path)?;
            println!("{}", serde_json::to_string_pretty(&engine.status()?)?);
        }
        Command::Search { query, path, limit } => {
            let engine = Engine::open(path)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&engine.search(&query, limit)?)?
            );
        }
        Command::Explore { query, path, limit } => {
            let engine = Engine::open(path)?;
            let hits = engine.explore(&query, limit)?;
            print!("{}", mcp::format_explore_text(&engine, &query, &hits)?);
        }
        Command::Callers {
            symbol,
            file,
            path,
            limit,
        } => {
            let engine = Engine::open(path)?;
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
            let engine = Engine::open(path)?;
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
            let engine = Engine::open(path)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&engine.impact_named(
                    &symbol,
                    file.as_deref(),
                    depth
                )?)?
            );
        }
        Command::Snapshot { path } => {
            let engine = Engine::open(path)?;
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
            let engine = Engine::open(path)?;
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
