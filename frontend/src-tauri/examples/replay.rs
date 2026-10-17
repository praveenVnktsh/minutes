//! Pipeline replay recorder: feeds a recorded meeting through the real live
//! pipeline and writes `events.jsonl` plus a pass/fail report.
//!
//! ```text
//! # from the repository root; needs a release llama-helper
//! cargo build --release -p llama-helper
//! cargo run --release -p meetily --features dev-replay --example replay -- \
//!     --audio "/path/to/Meeting …/audio.mp4"
//! ```
//!
//! Outputs go to `~/Library/Application Support/com.meetily.ai/replays/<run>/`
//! and stay there: recordings are private, never commit or upload them.
//! Open `events.jsonl` in the viewer (`site/replay/index.html`).
//!
//! The viewer, with the runs and a live Copilot, on http://127.0.0.1:7357:
//!
//! ```text
//! cargo run --release -p meetily --features dev-replay --example replay -- serve --open
//! ```
//!
//! llama-helper is found next to the binary or in the workspace's `target/`,
//! so neither needs `MEETILY_LLAMA_HELPER` (it still wins when set).
//!
//! See `app_lib::replay` for what is fed and recorded.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    about = "Replay a recorded meeting through the live pipeline",
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true
)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    /// The recording (audio.mp4 of a meeting folder).
    #[arg(long, required_unless_present = "rescore")]
    audio: Option<PathBuf>,
    /// Scripted Copilot steps (see app_lib::replay::script); a default
    /// script runs otherwise.
    #[arg(long)]
    script: Option<PathBuf>,
    /// The user's notes (live-notes.json); defaults to the one next to the audio.
    #[arg(long)]
    notes: Option<PathBuf>,
    /// Don't feed notes even if live-notes.json exists.
    #[arg(long)]
    no_notes: bool,
    /// Feed at real speed (slow; for occasional fidelity runs). By default
    /// the recording is fed as fast as live transcription decodes it, on a
    /// simulated meeting clock.
    #[arg(long)]
    realtime: bool,
    /// Put meeting text (detector lines, answers) in report.md.
    #[arg(long)]
    verbose: bool,
    /// How many times to run the summary (determinism check).
    #[arg(long, default_value_t = 2)]
    summary_runs: u32,
    /// Skip the final pass (and diarization).
    #[arg(long)]
    skip_final: bool,
    /// Feed only the first N seconds.
    #[arg(long)]
    limit_seconds: Option<f64>,
    /// A name for the run folder instead of a hash of the path.
    #[arg(long)]
    label: Option<String>,
    /// Re-score an existing events.jsonl (writes report.json/md next to it).
    #[arg(long)]
    rescore: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the viewer, the runs and a live Copilot on 127.0.0.1.
    Serve {
        #[arg(long, default_value_t = app_lib::replay::serve::DEFAULT_PORT)]
        port: u16,
        /// Open the viewer in the browser.
        #[arg(long)]
        open: bool,
        /// Serve the viewer from this folder (site/replay) instead of the
        /// copy built in, for working on it.
        #[arg(long)]
        site: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();
    // Before any thread starts: the sidecar lookup reads it.
    if app_lib::replay::use_local_llama_helper().is_none() {
        eprintln!("llama-helper not found: build it with `cargo build --release -p llama-helper` (the built-in model needs it)");
    }
    if let Some(Command::Serve { port, open, site }) = args.command {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
        let root = dirs::data_dir()
            .ok_or_else(|| anyhow::anyhow!("No data folder on this system"))?
            .join(app_lib::replay::runner::APP_IDENTIFIER)
            .join("replays");
        return app_lib::replay::serve::serve(root, port, open, site);
    }
    if let Some(events) = args.rescore {
        let records = app_lib::replay::events::parse_jsonl(&std::fs::read_to_string(&events)?)?;
        let report = app_lib::replay::report::score(&records);
        let dir = events.parent().map(PathBuf::from).unwrap_or_default();
        std::fs::write(dir.join("report.json"), serde_json::to_vec_pretty(&report)?)?;
        let markdown = app_lib::replay::report::to_markdown(&report, args.verbose);
        std::fs::write(dir.join("report.md"), &markdown)?;
        println!("{markdown}");
        return Ok(());
    }
    let out = app_lib::replay::runner::run(app_lib::replay::runner::Options {
        audio: args.audio.expect("--audio is required"),
        script: args.script,
        notes: args.notes,
        use_notes: !args.no_notes,
        realtime: args.realtime,
        verbose: args.verbose,
        summary_runs: args.summary_runs,
        skip_final: args.skip_final,
        limit_seconds: args.limit_seconds,
        label: args.label,
    })?;
    if args.verbose {
        let records = app_lib::replay::events::parse_jsonl(&std::fs::read_to_string(
            out.join("events.jsonl"),
        )?)?;
        let report = app_lib::replay::report::score(&records);
        std::fs::write(
            out.join("report.md"),
            app_lib::replay::report::to_markdown(&report, true),
        )?;
    }
    println!("Replay written to {}", out.display());
    Ok(())
}
