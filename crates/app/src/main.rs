//! `212` — entry point.
//!
//! Interactive by default. With `-p` or piped stdin it runs headless and
//! streams the reply to stdout, which makes 212 scriptable and testable.

mod headless;

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use common::AppPaths;
use common::proto::{Event, Request};
use config::Config;
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(name = "212", version, about = "A local-first AI terminal agent", long_about = None)]
struct Cli {
    /// Send one prompt, print the reply and exit (reads stdin if piped).
    #[arg(short, long)]
    print: Option<String>,
    /// Model id from the local store to use for this run.
    #[arg(short, long)]
    model: Option<String>,
    /// Provider for this run: local, ollama, openai, anthropic, compat.
    #[arg(long)]
    provider: Option<String>,
    /// Theme for this run.
    #[arg(long)]
    theme: Option<String>,
    /// Disable animations for this run.
    #[arg(long)]
    no_animations: bool,
    /// Force ASCII-only glyphs.
    #[arg(long)]
    ascii: bool,
    /// Answer yes to download/install confirmations in headless commands.
    #[arg(short, long, global = true)]
    yes: bool,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run diagnostics.
    Doctor,
    /// Manage local models.
    Models {
        #[command(subcommand)]
        action: Option<ModelsCmd>,
    },
    /// Install the verified llama.cpp backend.
    Backend,
    /// Print configuration and data locations.
    Paths,
}

#[derive(Subcommand)]
enum ModelsCmd {
    /// List installed models.
    List,
    /// Import a GGUF file, or `ollama:<name[:tag]>` from a local Ollama install.
    Import { source: String },
    /// Download from Hugging Face (`owner/repo[:QUANT]`) or `ollama:<name[:tag]>`.
    Pull { spec: String },
    /// Search Hugging Face for GGUF models.
    Search { query: String },
    /// Show model details.
    Info { id: String },
    /// Remove a model.
    Remove { id: String },
    /// Show models recommended for this machine.
    Recommend,
}

fn init_logging(paths: &AppPaths) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let filter = tracing_subscriber::EnvFilter::try_from_env("TWO_ONE_TWO_LOG").unwrap_or_else(|_| "info".into());
    let appender = tracing_appender::rolling::never(paths.log_dir(), "212.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt().with_env_filter(filter).with_writer(writer).with_ansi(false).init();
    Some(guard)
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let paths = AppPaths::discover();
    paths.ensure().with_context(|| format!("creating {}", paths.data_dir.display()))?;
    let guard = init_logging(&paths);

    let config_path = paths.config_file();
    let loaded = Config::load(&config_path)?;
    if loaded.first_run {
        Config::write_default(&config_path)?;
        let themes = paths.themes_dir();
        let _ = std::fs::create_dir_all(&themes);
        let _ = std::fs::write(themes.join("example.toml.sample"), tui::theme::EXAMPLE_THEME);
    }
    let mut config = loaded.config.clone();
    if let Some(p) = &cli.provider {
        config.inference.provider = p.clone();
    }
    if let Some(m) = &cli.model {
        config.inference.default_model = Some(m.clone());
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    let ok = rt.block_on(async move {
        let stdin_piped = !std::io::stdin().is_terminal();
        let interactive = cli.command.is_none() && cli.print.is_none() && !stdin_piped && std::io::stdout().is_terminal();
        let (req_tx, req_rx) = mpsc::channel::<Request>(256);
        let (ev_tx, ev_rx) = mpsc::channel::<Event>(1024);
        let needs_model = cli.command.is_none();
        let runtime = runtime::Runtime::new(
            runtime::RuntimeOptions {
                paths: paths.clone(),
                config: config.clone(),
                config_path: config_path.clone(),
                cwd: cwd.clone(),
                autoload: needs_model,
            },
            ev_tx,
        )
        .map_err(anyhow::Error::msg)?;
        let rt_task = tokio::spawn(runtime.run(req_rx, needs_model));
        spawn_signal_handler(req_tx.clone());

        let result = if interactive {
            let mut ui = config.ui.clone();
            if let Some(t) = &cli.theme {
                ui.theme = t.clone();
            }
            if cli.no_animations {
                ui.animations = config::AnimationMode::Off;
            }
            if cli.ascii {
                ui.glyphs = "ascii".into();
            }
            let opts = tui::TuiOptions {
                ui,
                themes_dir: paths.themes_dir(),
                history_file: paths.history_file(),
                config_path: config_path.clone(),
                first_run: loaded.first_run,
                warnings: loaded.warnings.clone(),
                cwd,
                keyboard_enhanced: false,
            };
            tui::run(opts, req_tx.clone(), ev_rx).await.map(|_| true).map_err(anyhow::Error::from)
        } else {
            for w in &loaded.warnings {
                eprintln!("warning: {w}");
            }
            let prompt = match (&cli.command, &cli.print) {
                (None, Some(p)) => Some(p.clone()),
                (None, None) => {
                    use std::io::Read;
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    Some(s)
                }
                _ => None,
            };
            let job = match (&cli.command, prompt) {
                (Some(Cmd::Doctor), _) => headless::Job::Doctor,
                (Some(Cmd::Backend), _) => headless::Job::Request(Request::InstallBackend),
                (Some(Cmd::Paths), _) => {
                    println!("config   {}", config_path.display());
                    println!("data     {}", paths.data_dir.display());
                    println!("models   {}", paths.default_models_dir().display());
                    println!("backends {}", paths.backends_dir().display());
                    println!("logs     {}", paths.log_dir().display());
                    headless::Job::Nothing
                }
                (Some(Cmd::Models { action }), _) => headless::Job::Request(match action {
                    None | Some(ModelsCmd::List) => Request::ListModels,
                    Some(ModelsCmd::Import { source }) => Request::ImportModel { source: source.clone() },
                    Some(ModelsCmd::Pull { spec }) => Request::Pull { spec: spec.clone() },
                    Some(ModelsCmd::Search { query }) => Request::Search { query: query.clone() },
                    Some(ModelsCmd::Info { id }) => Request::ModelInfo { id: id.clone() },
                    Some(ModelsCmd::Remove { id }) => Request::RemoveModel { id: id.clone() },
                    Some(ModelsCmd::Recommend) => Request::Recommend,
                }),
                (None, Some(p)) => headless::Job::Prompt(p),
                (None, None) => headless::Job::Nothing,
            };
            headless::run(job, cli.yes, req_tx.clone(), ev_rx).await
        };
        let _ = req_tx.send(Request::Shutdown).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(6), rt_task).await;
        result
    })?;
    drop(guard); // flush logs
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}

/// Translate SIGTERM/SIGHUP (and SIGINT when not in raw mode) into a clean
/// shutdown so the inference backend is always stopped.
fn spawn_signal_handler(tx: mpsc::Sender<Request>) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let (Ok(mut term), Ok(mut hup), Ok(mut int)) =
                (signal(SignalKind::terminate()), signal(SignalKind::hangup()), signal(SignalKind::interrupt()))
            else {
                return;
            };
            tokio::select! {
                _ = term.recv() => {}
                _ = hup.recv() => {}
                _ = int.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        tracing::info!("signal received; shutting down");
        let _ = tx.send(Request::Shutdown).await;
    });
}
