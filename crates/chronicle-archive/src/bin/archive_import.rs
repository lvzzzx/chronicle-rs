use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use chronicle_core::{Queue, WriterConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_usage();
        return Ok(());
    }

    let config = parse_args(&args)?;

    let mut writer = Queue::open_publisher_with_config(
        &config.archive_path,
        WriterConfig::archive(),
    )?;

    let file = File::open(&config.input_path)?;
    let reader = BufReader::new(file);

    let mut count: u64 = 0;
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let (timestamp_ns, payload) = parse_line(&line);
        match timestamp_ns {
            Some(ts) => writer.append_with_timestamp(config.type_id, payload.as_bytes(), ts)?,
            None => writer.append(config.type_id, payload.as_bytes())?,
        }
        count = count.wrapping_add(1);
        if count % 100000 == 0 {
            println!("archive-import: imported {count} lines");
        }
    }

    writer.flush_sync()?;
    println!("archive-import: completed {count} lines");
    Ok(())
}

struct CliConfig {
    archive_path: PathBuf,
    input_path: PathBuf,
    type_id: u16,
}

fn parse_args(args: &[String]) -> Result<CliConfig, String> {
    let mut archive_path: Option<PathBuf> = None;
    let mut input_path: Option<PathBuf> = None;
    let mut type_id: u16 = 1;

    let mut i = 1;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--archive" {
            i += 1;
            if i >= args.len() {
                return Err("missing value for --archive".to_string());
            }
            archive_path = Some(PathBuf::from(&args[i]));
        } else if let Some(value) = arg.strip_prefix("--archive=") {
            archive_path = Some(PathBuf::from(value));
        } else if arg == "--input" {
            i += 1;
            if i >= args.len() {
                return Err("missing value for --input".to_string());
            }
            input_path = Some(PathBuf::from(&args[i]));
        } else if let Some(value) = arg.strip_prefix("--input=") {
            input_path = Some(PathBuf::from(value));
        } else if arg == "--type-id" {
            i += 1;
            if i >= args.len() {
                return Err("missing value for --type-id".to_string());
            }
            type_id = parse_u16(&args[i])?;
        } else if let Some(value) = arg.strip_prefix("--type-id=") {
            type_id = parse_u16(value)?;
        } else {
            return Err(format!("unknown argument: {arg}"));
        }
        i += 1;
    }

    let archive_path = archive_path.ok_or_else(|| "--archive is required".to_string())?;
    let input_path = input_path.ok_or_else(|| "--input is required".to_string())?;

    Ok(CliConfig {
        archive_path,
        input_path,
        type_id,
    })
}

fn parse_u16(value: &str) -> Result<u16, String> {
    value
        .parse::<u16>()
        .map_err(|_| format!("invalid --type-id value: {value}"))
}

fn parse_line(line: &str) -> (Option<u64>, &str) {
    let trimmed = line.trim_end();
    let mut split = trimmed.splitn(2, |c: char| c == ' ' || c == '\t');
    let first = split.next().unwrap_or("");
    if let Some(rest) = split.next() {
        if let Ok(ts) = first.parse::<u64>() {
            return (Some(ts), rest.trim_start());
        }
    }
    (None, trimmed)
}

fn print_usage() {
    eprintln!("Usage: archive-import --archive <path> --input <file> [--type-id <u16>]");
    eprintln!("Each line is either '<timestamp_ns> <payload>' or just '<payload>'.");
}
