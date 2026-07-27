use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use structurely::{mcp, Engine};

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
        query: String,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    Explore {
        query: String,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    Callers {
        symbol: String,
        #[arg(long)]
        file: Option<String>,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    Callees {
        symbol: String,
        #[arg(long)]
        file: Option<String>,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    Impact {
        symbol: String,
        #[arg(long)]
        file: Option<String>,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 2)]
        depth: usize,
    },
    Snapshot {
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
    Benchmark {
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value = "main")]
        query: String,
        #[arg(long, default_value_t = 20)]
        iterations: usize,
    },
    Serve {
        #[arg(long)]
        mcp: bool,
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
            println!(
                "{}",
                serde_json::to_string_pretty(&engine.explore(&query, limit)?)?
            );
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
        Command::Serve { mcp: true, path } => mcp::serve_stdio(&path)?,
        Command::Serve { mcp: false, .. } => {
            anyhow::bail!("only `structurely serve --mcp` is currently supported")
        }
    }
    Ok(())
}
