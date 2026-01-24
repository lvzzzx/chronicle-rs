use std::env;
use std::path::PathBuf;
use std::time::Duration;

use chronicle_layout::IpcLayout;
use chronicle_core::Queue;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_usage();
        return Ok(());
    }

    let bus_root = parse_bus_root(&args)?;
    let layout = IpcLayout::new(&bus_root);
    let queue_path = layout
        .streams()
        .raw_queue_dir("market_data")
        .expect("invalid venue for raw queue path");
    println!(
        "feed: bus_root={} queue={}",
        bus_root.display(),
        queue_path.display()
    );

    let mut writer = Queue::open_publisher(&queue_path)?;
    let symbols = ["BTC", "ETH", "SOL"];
    let mut seq: u64 = 0;

    loop {
        let symbol = symbols[(seq as usize) % symbols.len()];
        let price = base_price(symbol) + ((seq % 100) as f64) * 0.5;
        let payload = format!("symbol={symbol} price={price:.2} seq={seq}");
        writer.append(1, payload.as_bytes())?;
        println!("feed: published {payload}");
        seq = seq.wrapping_add(1);
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn parse_bus_root(args: &[String]) -> Result<PathBuf, String> {
    let mut bus_root = PathBuf::from("./demo_bus");
    let mut i = 1;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--bus-root" {
            i += 1;
            if i >= args.len() {
                return Err("missing value for --bus-root".to_string());
            }
            bus_root = PathBuf::from(&args[i]);
        } else if let Some(value) = arg.strip_prefix("--bus-root=") {
            bus_root = PathBuf::from(value);
        } else {
            return Err(format!("unknown argument: {arg}"));
        }
        i += 1;
    }
    Ok(bus_root)
}

fn base_price(symbol: &str) -> f64 {
    match symbol {
        "BTC" => 30_000.0,
        "ETH" => 2_000.0,
        "SOL" => 100.0,
        _ => 1.0,
    }
}

fn print_usage() {
    eprintln!("Usage: feed [--bus-root <path>]");
}
