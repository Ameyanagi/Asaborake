//! The `asaborake` command.
//!
//! Automatic CM cutting and transcoding for Japanese broadcast MPEG-2 TS.
//! Heavily inspired by nekopanda's Amatsukaze — see `ATTRIBUTION.md`.

// Tests assert; asserting is how they fail. The workspace bans panicking
// constructs in shipping code, not in the suite that checks it.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used, clippy::panic))]

mod epgstation;

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use asaborake_core::profile::Profile;
use asaborake_core::{JobRequest, LogoStore};
use asaborake_media::Ffmpeg;
use clap::{Args, Parser, Subcommand};

/// The credit line, shown by `--version`, because it belongs everywhere the
/// project introduces itself.
const CREDIT: &str = "Heavily inspired by Amatsukaze (https://github.com/nekopanda/Amatsukaze)";

#[derive(Debug, Parser)]
#[command(
    name = "asaborake",
    version,
    about = "Automatic CM cutting and transcoding for Japanese broadcast TS",
    long_about = None,
    after_help = CREDIT,
)]
struct Cli {
    /// Path to the ffmpeg binary.
    #[arg(long, global = true, env = "ASABORAKE_FFMPEG")]
    ffmpeg: Option<PathBuf>,

    /// Path to the ffprobe binary.
    #[arg(long, global = true, env = "ASABORAKE_FFPROBE")]
    ffprobe: Option<PathBuf>,

    /// Directory holding learned logos.
    #[arg(long, global = true, env = "ASABORAKE_LOGO_DIR")]
    logo_dir: Option<PathBuf>,

    /// Increase log detail. Repeat for more.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Report what a recording contains, without decoding it.
    Probe {
        /// The recording to inspect.
        input: PathBuf,
        /// Emit JSON rather than a summary.
        #[arg(long)]
        json: bool,
    },

    /// Analyse a recording and report where the commercials are.
    Analyse {
        /// The recording to analyse.
        input: PathBuf,
        /// Write the analysis and plan here as JSON.
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[command(flatten)]
        context: RecordingContext,
    },

    /// Transcode a recording, cutting the commercials.
    Encode {
        /// The recording to transcode.
        input: PathBuf,
        /// Where to write the result.
        #[arg(short, long)]
        output: PathBuf,
        /// Encoding profile.
        #[arg(short, long, default_value = "nvenc-h264")]
        profile: String,
        /// Transcode the whole recording, marking commercials as chapters
        /// rather than removing them.
        #[arg(long)]
        no_cut: bool,
        #[command(flatten)]
        context: RecordingContext,
    },

    /// Run as an `EPGStation` external encoder, reading its environment.
    // The doc comment needs backticks for rustdoc; the help text must not
    // show them to an operator reading `--help`.
    #[command(about = "Run as an EPGStation external encoder, reading its environment")]
    Epgstation {
        /// Encoding profile.
        #[arg(short, long, default_value = "nvenc-h264")]
        profile: String,
        /// Transcode the whole recording, marking commercials as chapters.
        #[arg(long)]
        no_cut: bool,
    },

    /// Inspect and manage learned logos.
    Logo {
        #[command(subcommand)]
        command: LogoCommand,
    },

    /// List the available encoding profiles.
    Profiles,
}

/// Details about the recording that improve detection when they are known.
#[derive(Debug, Args, Clone, Default)]
struct RecordingContext {
    /// Channel id, used as the logo store key.
    #[arg(long)]
    channel_id: Option<String>,
    /// Channel name, used to label a newly learned logo.
    #[arg(long)]
    channel_name: Option<String>,
    /// Programme title, for logs and the cut record.
    #[arg(long)]
    title: Option<String>,
}

#[derive(Debug, Subcommand)]
enum LogoCommand {
    /// List the logos that have been learned.
    List,
    /// Write a PNG preview of a stored logo.
    Show {
        /// Channel id.
        channel_id: String,
        /// Source frame width.
        width: u32,
        /// Source frame height.
        height: u32,
        /// Where to write the PNG.
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Forget a stored logo, so it is learned again next time.
    Forget {
        /// Channel id.
        channel_id: String,
        /// Source frame width.
        width: u32,
        /// Source frame height.
        height: u32,
    },
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    init_logging(
        cli.verbose,
        matches!(cli.command, Command::Epgstation { .. }),
    );

    match run(&cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            // The chain matters: "media error" alone says nothing, and the
            // ffmpeg stderr tail is usually the sentence that explains it.
            tracing::error!("{error:#}");
            eprintln!("asaborake: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Probe { input, json } => probe(input, *json),
        Command::Analyse {
            input,
            output,
            context,
        } => analyse(cli, input, output.as_deref(), context),
        Command::Encode {
            input,
            output,
            profile,
            no_cut,
            context,
        } => encode(cli, input, output, profile, *no_cut, context),
        Command::Epgstation { profile, no_cut } => run_epgstation(cli, profile, *no_cut),
        Command::Logo { command } => logo(cli, command),
        Command::Profiles => {
            profiles();
            Ok(())
        }
    }
}

/// Set up logging.
///
/// Under `EPGStation`, stdout carries the progress protocol and nothing else,
/// so logs go to stderr — which `EPGStation` captures into its own debug log.
fn init_logging(verbosity: u8, epgstation: bool) {
    let level = match verbosity {
        0 => "asaborake=info",
        1 => "asaborake=debug",
        _ => "asaborake=trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("ASABORAKE_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(!epgstation && std::io::stderr().is_terminal())
        .try_init();
}

/// Locate ffmpeg, with a message that says what to do about it if absent.
fn ffmpeg(cli: &Cli) -> Result<Ffmpeg> {
    Ffmpeg::discover(cli.ffmpeg.as_deref(), cli.ffprobe.as_deref())
        .context("Asaborake drives ffmpeg; install it or pass --ffmpeg")
}

/// Open the logo store, if one was configured.
fn store(cli: &Cli) -> Result<Option<LogoStore>> {
    cli.logo_dir
        .as_ref()
        .map(|dir| {
            LogoStore::open(dir).with_context(|| format!("opening logo store {}", dir.display()))
        })
        .transpose()
}

/// Look up a profile by name.
fn profile(name: &str) -> Result<Profile> {
    asaborake_core::profile::builtin()
        .remove(name)
        .with_context(|| {
            let available: Vec<String> =
                asaborake_core::profile::builtin().keys().cloned().collect();
            format!(
                "no profile named '{name}'; available: {}",
                available.join(", ")
            )
        })
}

fn probe(input: &Path, json: bool) -> Result<()> {
    let file =
        std::fs::File::open(input).with_context(|| format!("opening {}", input.display()))?;
    let size = file.metadata().map_or(0, |m| m.len());
    let info = asaborake_ts::scan(std::io::BufReader::new(file), size)
        .with_context(|| format!("reading {} as a transport stream", input.display()))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&info)?);
        return Ok(());
    }

    println!("layout        {:?}", info.layout);
    println!("packets       {}", info.packet_count);
    println!("duration      {:.1}s", info.duration_seconds);
    if let Some(format) = info.video_format {
        println!(
            "video         {}x{} @ {:.3} fps{}",
            format.width,
            format.height,
            format.fps(),
            if format.interlaced { " interlaced" } else { "" }
        );
    }
    for program in &info.programs {
        println!("program {}", program.program_number);
        for stream in &program.streams {
            println!("  pid {:#06x}  {:?}", stream.pid, stream.kind);
        }
    }
    println!(
        "health        {} dropped, {} scrambled, {} errors",
        info.stats.dropped_packets, info.stats.scrambled_packets, info.stats.error_packets
    );
    if info.requires_split() {
        println!("note          the picture geometry changes mid-recording");
    }
    Ok(())
}

fn analyse(
    cli: &Cli,
    input: &Path,
    output: Option<&Path>,
    context: &RecordingContext,
) -> Result<()> {
    let ffmpeg = ffmpeg(cli)?;
    let store = store(cli)?;
    let probe = asaborake_media::probe(&ffmpeg, input)?;
    let video = probe
        .video
        .as_ref()
        .context("the recording has no video stream")?;

    let stored = store
        .as_ref()
        .zip(context.channel_id.as_deref())
        .and_then(|(store, channel)| store.load(channel, video.width, video.height));

    let options = asaborake_analyze::AnalysisOptions {
        logo: stored,
        logo_name: context
            .channel_name
            .clone()
            .unwrap_or_else(|| "unknown".to_owned()),
        channel_id: context.channel_id.clone(),
        deinterlace: video.interlaced,
        ..asaborake_analyze::AnalysisOptions::default()
    };

    let mut last = String::new();
    let analysis = asaborake_analyze::analyse(&ffmpeg, input, &options, &mut |progress| {
        let stage = format!("{:?}", progress.stage);
        if stage != last {
            eprintln!("{stage}");
            last = stage;
        }
    })?;

    if let (Some(store), Some(logo)) = (store.as_ref(), analysis.learned_logo.as_ref()) {
        store.save(logo)?;
    }

    let plan = asaborake_cmcut::plan(&analysis, &asaborake_cmcut::CutOptions::default());

    if let Some(path) = output {
        let document = serde_json::json!({ "analysis": analysis, "plan": plan });
        std::fs::write(path, serde_json::to_string_pretty(&document)?)
            .with_context(|| format!("writing {}", path.display()))?;
        eprintln!("wrote {}", path.display());
    }

    print_plan(&analysis, &plan);
    Ok(())
}

fn print_plan(analysis: &asaborake_analyze::Analysis, plan: &asaborake_cmcut::CutPlan) {
    println!("duration      {:.1}s", analysis.duration_seconds);
    match &analysis.logo {
        Some(logo) => println!(
            "logo          {}x{} at ({}, {}), opacity {:.2}, {} present",
            logo.rect.width,
            logo.rect.height,
            logo.rect.x,
            logo.rect.y,
            logo.mean_alpha,
            format_args!("{:.0}%", analysis.logo_coverage() * 100.0),
        ),
        None => println!("logo          not found"),
    }
    println!(
        "signals       {} scene changes, {} silences",
        analysis.scene_changes.len(),
        analysis.silent_spans.len()
    );
    println!("confidence    {:.2}", plan.confidence);
    println!("decision      {:?} — {}", plan.decision, plan.reason);

    for segment in &plan.segments {
        println!(
            "  {:>8.1}s  {:>8.1}s  {:<10} {:.2}",
            segment.start,
            segment.end,
            segment.kind.label(),
            segment.confidence
        );
    }
    println!(
        "would remove  {:.1}s of {:.1}s",
        plan.cut_seconds(),
        analysis.duration_seconds
    );
}

fn encode(
    cli: &Cli,
    input: &Path,
    output: &Path,
    profile_name: &str,
    no_cut: bool,
    context: &RecordingContext,
) -> Result<()> {
    let ffmpeg = ffmpeg(cli)?;
    let store = store(cli)?;
    let request = build_request(input, output, profile_name, no_cut, context)?;

    let mut last = String::new();
    let outcome = asaborake_core::run(&ffmpeg, store.as_ref(), &request, &mut |progress| {
        if progress.message != last {
            eprintln!("{:>5.1}%  {}", progress.fraction * 100.0, progress.message);
            last.clone_from(&progress.message);
        }
    })?;

    println!("wrote {}", outcome.output.display());
    println!("cut record {}", outcome.sidecar.display());
    println!(
        "removed {:.1}s, confidence {:.2}",
        outcome.plan.cut_seconds(),
        outcome.plan.confidence
    );
    Ok(())
}

/// Assemble a job request shared by `encode` and `epgstation`.
fn build_request(
    input: &Path,
    output: &Path,
    profile_name: &str,
    no_cut: bool,
    context: &RecordingContext,
) -> Result<JobRequest> {
    let mut request = JobRequest::new(input, output, profile(profile_name)?);
    request.channel_id.clone_from(&context.channel_id);
    request.channel_name.clone_from(&context.channel_name);
    request.title.clone_from(&context.title);
    if no_cut {
        // Detection still runs, and its result still becomes chapters; only
        // the removal is suppressed.
        request.cut.low_confidence = asaborake_cmcut::LowConfidencePolicy::Keep;
        request.cut.confidence_threshold = f64::INFINITY;
    }
    Ok(request)
}

fn run_epgstation(cli: &Cli, profile_name: &str, no_cut: bool) -> Result<()> {
    let environment = epgstation::Environment::from_env();
    let input = environment
        .input
        .clone()
        .context("EPGStation did not set INPUT; is this running as an encoder?")?;
    let output = environment
        .output
        .clone()
        .context("EPGStation did not set OUTPUT; the encode entry needs a suffix")?;

    tracing::info!(
        recorded_id = environment.recorded_id.as_deref().unwrap_or("?"),
        name = environment.name.as_deref().unwrap_or("?"),
        "starting EPGStation job"
    );

    let ffmpeg = ffmpeg(cli)?;
    let store = store(cli)?;
    let context = RecordingContext {
        channel_id: environment.channel_id.clone(),
        channel_name: environment.channel_name.clone(),
        title: environment.name.clone(),
    };
    let request = build_request(&input, &output, profile_name, no_cut, &context)?;

    let mut reporter = epgstation::ProgressReporter::new();
    let mut stdout = std::io::stdout();
    reporter.report(&mut stdout, 0.0, "starting");

    let outcome = asaborake_core::run(&ffmpeg, store.as_ref(), &request, &mut |progress| {
        reporter.report(&mut stdout, progress.fraction, &progress.message);
    });

    match outcome {
        Ok(outcome) => {
            reporter.report(&mut stdout, 1.0, "done");
            tracing::info!(
                removed = outcome.plan.cut_seconds(),
                confidence = outcome.plan.confidence,
                "finished"
            );
            Ok(())
        }
        Err(error) => {
            // Exiting non-zero is what tells EPGStation to delete the partial
            // output, so the error must propagate rather than be swallowed.
            bail!(error)
        }
    }
}

fn logo(cli: &Cli, command: &LogoCommand) -> Result<()> {
    let store = store(cli)?.context("pass --logo-dir to work with stored logos")?;

    match command {
        LogoCommand::List => {
            let logos = store.list()?;
            if logos.is_empty() {
                println!(
                    "no logos learned yet (they appear after the first recording per channel)"
                );
                return Ok(());
            }
            for logo in logos {
                println!(
                    "{:<24} {}x{}  rect {}x{}+{}+{}  opacity {:.2}  {} frames",
                    logo.channel_id.as_deref().unwrap_or("?"),
                    logo.source_width,
                    logo.source_height,
                    logo.rect.width,
                    logo.rect.height,
                    logo.rect.x,
                    logo.rect.y,
                    logo.mean_alpha(),
                    logo.frames_used,
                );
            }
            Ok(())
        }
        LogoCommand::Show {
            channel_id,
            width,
            height,
            output,
        } => {
            let logo = store
                .load(channel_id, *width, *height)
                .with_context(|| format!("no logo for channel {channel_id} at {width}x{height}"))?;
            logo.write_png(output)?;
            println!("wrote {}", output.display());
            Ok(())
        }
        LogoCommand::Forget {
            channel_id,
            width,
            height,
        } => {
            if store.remove(channel_id, *width, *height)? {
                println!("forgot the logo for channel {channel_id} at {width}x{height}");
            } else {
                println!("no logo for channel {channel_id} at {width}x{height}");
            }
            Ok(())
        }
    }
}

fn profiles() {
    for (name, profile) in asaborake_core::profile::builtin() {
        println!(
            "{name:<12} {:<12} {}",
            profile.video.encoder, profile.description
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;

    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn the_epgstation_subcommand_needs_no_arguments() {
        // `EPGStation` invokes the binary with whatever is in config.yml and
        // supplies everything else through the environment, so the common
        // form must parse with only a profile.
        let cli = Cli::try_parse_from(["asaborake", "epgstation"]).expect("parses");
        assert!(matches!(cli.command, Command::Epgstation { .. }));
    }

    #[test]
    fn encode_requires_an_output() {
        assert!(Cli::try_parse_from(["asaborake", "encode", "in.ts"]).is_err());
        assert!(Cli::try_parse_from(["asaborake", "encode", "in.ts", "-o", "out.mp4"]).is_ok());
    }

    #[test]
    fn an_unknown_profile_lists_the_ones_that_exist() {
        let error = profile("no-such-profile").expect_err("must fail");
        let message = format!("{error}");
        assert!(message.contains("nvenc-h264"), "unhelpful: {message}");
    }

    #[test]
    fn no_cut_suppresses_removal_without_suppressing_detection() {
        let request = build_request(
            Path::new("in.ts"),
            Path::new("out.mp4"),
            "x264-cpu",
            true,
            &RecordingContext::default(),
        )
        .expect("builds");
        assert!(request.cut.confidence_threshold.is_infinite());
        assert!(request.learn_logo, "detection still runs");
    }

    #[test]
    fn the_version_banner_credits_amatsukaze() {
        assert!(CREDIT.contains("Amatsukaze"));
        assert!(CREDIT.contains("nekopanda"));
    }
}
