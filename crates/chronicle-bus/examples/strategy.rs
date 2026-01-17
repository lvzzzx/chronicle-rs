use std::env;
use std::path::PathBuf;
use std::time::Duration;

use chronicle_bus::{BusLayout, StrategyId};
use chronicle_core::{Error, Queue, QueueReader};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_usage();
        return Ok(());
    }

    let options = parse_args(&args)?;
    let strategy_id = StrategyId(options.strategy.clone());
    let layout = BusLayout::new(&options.bus_root);
    let endpoints = layout.strategy_endpoints(&strategy_id);

    println!(
        "strategy {}: bus_root={} symbol={}",
        strategy_id.0,
        options.bus_root.display(),
        options.symbol
    );

    let feed_path = options
        .bus_root
        .join("market_data")
        .join("queue")
        .join("demo_feed");

    let reader_name = format!("reader_{}", strategy_id.0);
    let mut feed_reader = open_subscriber_retry(&feed_path, &reader_name)?;

    let mut orders_out_writer = Queue::open_publisher(&endpoints.orders_out)?;
    layout.mark_ready(&endpoints.orders_out)?;
    println!(
        "strategy {}: READY {}",
        strategy_id.0,
        endpoints.orders_out.display()
    );

    let mut orders_in_reader: Option<QueueReader> = None;
    let mut order_seq: u64 = 0;

    loop {
        feed_reader.wait(Some(Duration::from_millis(100)))?;
        while let Some(msg) = feed_reader.next()? {
            let text = std::str::from_utf8(msg.payload).unwrap_or("");
            if payload_symbol_matches(text, &options.symbol) {
                let order_id = order_seq;
                let payload = format!(
                    "order_id={order_id} symbol={} qty=1 side=BUY",
                    options.symbol
                );
                orders_out_writer.append(2, payload.as_bytes())?;
                println!("strategy {}: order {payload}", strategy_id.0);
                order_seq = order_seq.wrapping_add(1);
            }
            feed_reader.commit()?;
        }

        if orders_in_reader.is_none() {
            match Queue::open_subscriber(&endpoints.orders_in, &reader_name) {
                Ok(reader) => {
                    println!(
                        "strategy {}: attached to {}",
                        strategy_id.0,
                        endpoints.orders_in.display()
                    );
                    orders_in_reader = Some(reader);
                }
                Err(Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(err) => return Err(Box::new(err)),
            }
        }

        if let Some(reader) = orders_in_reader.as_mut() {
            while let Some(msg) = reader.next()? {
                let text = std::str::from_utf8(msg.payload).unwrap_or("");
                println!("strategy {}: ack {text}", strategy_id.0);
                reader.commit()?;
            }
        }
    }
}

struct Options {
    bus_root: PathBuf,
    strategy: String,
    symbol: String,
}

fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut bus_root = PathBuf::from("./demo_bus");
    let mut strategy = "strategy_a".to_string();
    let mut symbol = "BTC".to_string();
    let mut i = 1;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--bus-root" => {
                i += 1;
                if i >= args.len() {
                    return Err("missing value for --bus-root".to_string());
                }
                bus_root = PathBuf::from(&args[i]);
            }
            "--strategy" => {
                i += 1;
                if i >= args.len() {
                    return Err("missing value for --strategy".to_string());
                }
                strategy = args[i].clone();
            }
            "--symbol" => {
                i += 1;
                if i >= args.len() {
                    return Err("missing value for --symbol".to_string());
                }
                symbol = args[i].clone();
            }
            _ => {
                if let Some(value) = arg.strip_prefix("--bus-root=") {
                    bus_root = PathBuf::from(value);
                } else if let Some(value) = arg.strip_prefix("--strategy=") {
                    strategy = value.to_string();
                } else if let Some(value) = arg.strip_prefix("--symbol=") {
                    symbol = value.to_string();
                } else {
                    return Err(format!("unknown argument: {arg}"));
                }
            }
        }
        i += 1;
    }
    Ok(Options {
        bus_root,
        strategy,
        symbol: symbol.to_ascii_uppercase(),
    })
}

fn open_subscriber_retry(path: &PathBuf, reader_name: &str) -> Result<QueueReader, Box<dyn std::error::Error>> {
    loop {
        match Queue::open_subscriber(path, reader_name) {
            Ok(reader) => return Ok(reader),
            Err(Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(err) => return Err(Box::new(err)),
        }
    }
}

fn payload_symbol_matches(payload: &str, symbol: &str) -> bool {
    payload
        .split_whitespace()
        .filter_map(|part| part.strip_prefix("symbol="))
        .any(|value| value == symbol)
}

fn print_usage() {
    eprintln!(
        "Usage: strategy [--bus-root <path>] [--strategy <id>] [--symbol <SYM>]\n\
Defaults: --bus-root ./demo_bus --strategy strategy_a --symbol BTC"
    );
}
