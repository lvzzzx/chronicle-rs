use std::env;
use std::path::PathBuf;

use chronicle_storage::ArchiveTap;
use chronicle_core::StartMode;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_usage();
        return Ok(());
    }

    let config = parse_args(&args)?;
    println!(
        "archive-tap: live={} archive={} reader={} start={:?}",
        config.live_path.display(),
        config.archive_path.display(),
        config.reader_name,
        config.start_mode
    );

    let tap = ArchiveTap::new(config.live_path, config.archive_path)
        .reader_name(config.reader_name)
        .start_mode(config.start_mode);

    tap.run()?;
    Ok(())
}

struct CliConfig {
    live_path: PathBuf,
    archive_path: PathBuf,
    reader_name: String,
    start_mode: StartMode,
}

fn parse_args(args: &[String]) -> Result<CliConfig, String> {
    let mut live_path: Option<PathBuf> = None;
    let mut archive_path: Option<PathBuf> = None;
    let mut reader_name = "archive_tap".to_string();
    let mut start_mode = StartMode::ResumeStrict;

    let mut i = 1;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--live" {
            i += 1;
            if i >= args.len() {
                return Err("missing value for --live".to_string());
            }
            live_path = Some(PathBuf::from(&args[i]));
        } else if let Some(value) = arg.strip_prefix("--live=") {
            live_path = Some(PathBuf::from(value));
        } else if arg == "--archive" {
            i += 1;
            if i >= args.len() {
                return Err("missing value for --archive".to_string());
            }
            archive_path = Some(PathBuf::from(&args[i]));
        } else if let Some(value) = arg.strip_prefix("--archive=") {
            archive_path = Some(PathBuf::from(value));
        } else if arg == "--reader" {
            i += 1;
            if i >= args.len() {
                return Err("missing value for --reader".to_string());
            }
            reader_name = args[i].to_string();
        } else if let Some(value) = arg.strip_prefix("--reader=") {
            reader_name = value.to_string();
        } else if arg == "--start" {
            i += 1;
            if i >= args.len() {
                return Err("missing value for --start".to_string());
            }
            start_mode = parse_start_mode(&args[i])?;
        } else if let Some(value) = arg.strip_prefix("--start=") {
            start_mode = parse_start_mode(value)?;
        } else {
            return Err(format!("unknown argument: {arg}"));
        }
        i += 1;
    }

    let live_path = live_path.ok_or_else(|| "--live is required".to_string())?;
    let archive_path = archive_path.ok_or_else(|| "--archive is required".to_string())?;

    Ok(CliConfig {
        live_path,
        archive_path,
        reader_name,
        start_mode,
    })
}

fn parse_start_mode(value: &str) -> Result<StartMode, String> {
    match value {
        "resume" | "resume-strict" => Ok(StartMode::ResumeStrict),
        "resume-snapshot" => Ok(StartMode::ResumeSnapshot),
        "resume-latest" => Ok(StartMode::ResumeLatest),
        "latest" => Ok(StartMode::Latest),
        "earliest" => Ok(StartMode::Earliest),
        _ => Err(format!(
            "invalid --start value: {value}. Use resume-strict, resume-snapshot, resume-latest, latest, or earliest."
        )),
    }
}

fn print_usage() {
    eprintln!(
        "Usage: archive-tap --live <path> --archive <path> [--reader <name>] [--start <mode>]"
    );
    eprintln!("  --start modes: resume-strict|resume-snapshot|resume-latest|latest|earliest");
}
