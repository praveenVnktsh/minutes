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
//! See `app_lib::replay` for what is fed and recorded.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(about = "Replay a recorded meeting through the live pipeline")]
struct Args {
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

fn main() -> Result<()> {
    let args = Args::parse();
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
