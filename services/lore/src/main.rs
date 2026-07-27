//! Lore CLI entrypoint.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use lore_service::{
    CorpusRegistry, IndexOptions, ServeOptions, eval_command, export_command, index_command,
    run_watcher, serve_http,
};

#[derive(Parser)]
#[command(
    name = "lore",
    version,
    about = "Structure-aware markdown retrieval for agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build a corpus index from a directory of markdown files.
    Index {
        /// Corpus root directory.
        path: PathBuf,
        /// Override the corpus identifier. Defaults to the root's basename.
        #[arg(long)]
        source_id: Option<String>,
        /// Emit a JSON summary to stdout on success.
        #[arg(long)]
        json: bool,
    },
    /// Serve indexed corpora over the MCP Streamable HTTP transport.
    Serve {
        /// One or more corpus roots to load at startup. Each must already
        /// have been indexed with `lore index`.
        #[arg(short = 'r', long = "root", value_name = "DIR")]
        roots: Vec<PathBuf>,
        /// Address to bind the HTTP listener to.
        #[arg(long, default_value = "127.0.0.1:7331")]
        bind: SocketAddr,
        /// Path prefix under which MCP is mounted.
        #[arg(long, default_value = "/mcp")]
        path: String,
    },
    /// Export an `llms.txt` map of a corpus (and `llms-full.txt` with
    /// `--full`) — a link-first index usable by agents that don't speak MCP.
    Export {
        /// Corpus root. Indexed on the fly if `.lore/index.json` is absent.
        #[arg(short = 'r', long = "root", value_name = "DIR")]
        root: PathBuf,
        /// Directory to write `llms.txt` (+ `llms-full.txt`) into. Omit to
        /// print `llms.txt` to stdout.
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
        /// Also write `llms-full.txt` with every document's full text inline.
        #[arg(long)]
        full: bool,
    },
    /// Measure retrieval effectiveness: run a labeled query set through the
    /// ranker and report Success@k, MRR, and coverage-verdict accuracy.
    Eval {
        /// Corpus root. Indexed on the fly if `.lore/index.json` is absent.
        #[arg(short = 'r', long = "root", value_name = "DIR")]
        root: PathBuf,
        /// JSONL query set: one `{query, relevant, expected_coverage?}` per line.
        #[arg(short = 'q', long = "queries", value_name = "FILE")]
        queries: PathBuf,
        /// Depth to search when locating the first relevant hit.
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Emit the full summary as JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Serve corpora *and* watch their roots for changes, re-indexing
    /// affected files on the fly.
    Watch {
        /// One or more corpus roots to load + watch.
        #[arg(short = 'r', long = "root", value_name = "DIR")]
        roots: Vec<PathBuf>,
        /// Address to bind the HTTP listener to.
        #[arg(long, default_value = "127.0.0.1:7331")]
        bind: SocketAddr,
        /// Path prefix under which MCP is mounted.
        #[arg(long, default_value = "/mcp")]
        path: String,
        /// Debounce window in milliseconds.
        #[arg(long, default_value_t = 250u64)]
        debounce_ms: u64,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("lore=info,lore_service=info")
            }),
        )
        .init();

    let cli = Cli::parse();
    let outcome = match cli.command {
        Command::Index {
            path,
            source_id,
            json,
        } => run_index(path, source_id, json),
        Command::Serve { roots, bind, path } => run_serve(roots, bind, path, None).await,
        Command::Export { root, out, full } => export_command(&root, out.as_deref(), full),
        Command::Eval {
            root,
            queries,
            limit,
            json,
        } => run_eval_cmd(root, queries, limit, json),
        Command::Watch {
            roots,
            bind,
            path,
            debounce_ms,
        } => run_serve(roots, bind, path, Some(Duration::from_millis(debounce_ms))).await,
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lore: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_eval_cmd(
    root: PathBuf,
    queries: PathBuf,
    limit: usize,
    json: bool,
) -> lore_core::Result<()> {
    let summary = eval_command(&root, &queries, limit)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    println!(
        "eval: {total} queries ({judged} judged, {cov} coverage-scored)",
        total = summary.total,
        judged = summary.judged,
        cov = summary.coverage_scored,
    );
    println!("  Success@1   {:.3}", summary.success_at_1);
    println!("  Success@3   {:.3}", summary.success_at_3);
    println!("  Success@10  {:.3}", summary.success_at_10);
    println!("  MRR         {:.3}", summary.mrr);
    println!(
        "  Coverage    {:.3} ({}/{})",
        summary.coverage_accuracy, summary.coverage_correct, summary.coverage_scored,
    );
    println!();
    println!("  rank  cov      query");
    for r in &summary.results {
        let rank = match r.rank {
            Some(n) => format!("{n}"),
            None if r.judged => "MISS".to_string(),
            None => "-".to_string(),
        };
        let cov = match r.coverage_ok {
            Some(true) => format!("{} ok", r.coverage),
            Some(false) => format!("{} BAD", r.coverage),
            None => r.coverage.clone(),
        };
        println!("  {rank:<5} {cov:<8} {}", r.query);
    }
    Ok(())
}

fn run_index(path: PathBuf, source_id: Option<String>, json: bool) -> lore_core::Result<()> {
    let mut opts = IndexOptions::new(path);
    opts.source_id = source_id;
    let report = index_command(opts)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "indexed {files} files ({nodes} headings) into {path} in {build}ms (write {write}ms)",
            files = report.files_indexed,
            nodes = report.total_nodes,
            path = report.index_path.display(),
            build = report.build_millis,
            write = report.write_millis,
        );
        if report.files_failed > 0 {
            println!("{} file(s) failed to parse", report.files_failed);
        }
    }
    Ok(())
}

/// Load every root into a shared `CorpusRegistry`, optionally spawn a
/// file watcher, and serve the MCP HTTP endpoint until the listener
/// errors out. `watch_debounce = Some(..)` enables watch mode; `None`
/// just serves.
async fn run_serve(
    roots: Vec<PathBuf>,
    bind: SocketAddr,
    path: String,
    watch_debounce: Option<Duration>,
) -> lore_core::Result<()> {
    let registry = CorpusRegistry::new();
    for root in &roots {
        registry.load_from_root(root)?;
    }

    if let Some(debounce) = watch_debounce {
        let reg = registry.clone();
        tokio::spawn(async move {
            if let Err(e) = run_watcher(reg, debounce).await {
                tracing::warn!(err = %e, "watcher exited");
            }
        });
    }

    // Periodically persist decayed access counts (KB-644). Dirty-gated, so an
    // idle server writes nothing. A hard kill loses at most one interval of
    // bumps — acceptable for a hotness signal that's regenerable anyway.
    {
        let reg = registry.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(30));
            tick.tick().await; // first tick fires immediately; skip it
            loop {
                tick.tick().await;
                if let Err(e) = reg.flush_access() {
                    tracing::warn!(err = %e, "access-store flush failed");
                }
            }
        });
    }

    serve_http(registry, ServeOptions { bind, path })
        .await
        .map_err(|e| lore_core::Error::Io(e.to_string()))
}
