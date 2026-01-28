use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::time::Duration;

use chronicle::bus::mark_ready;
use chronicle::trading::{DiscoveryEvent, RouterDiscovery, StrategyId};
use chronicle::core::{Queue, QueueReader, QueueWriter};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_usage();
        return Ok(());
    }

    let bus_root = parse_bus_root(&args)?;
    let mut discovery = RouterDiscovery::new(&bus_root)?;

    println!("router: bus_root={}", discovery.root().display());

    let mut handles: HashMap<StrategyId, StrategyHandle> = HashMap::new();

    loop {
        let mut idle = true;
        let events = discovery.poll()?;
        if !events.is_empty() {
            idle = false;
        }
        for event in events {
            match event {
                DiscoveryEvent::Added {
                    strategy,
                    orders_out,
                } => {
                    if handles.contains_key(&strategy) {
                        continue;
                    }
                    let endpoints = discovery
                        .strategy_endpoints(&strategy)
                        .expect("invalid strategy id for orders endpoints");
                    let reader = Queue::open_subscriber(&orders_out, "router")?;
                    let writer = Queue::open_publisher(&endpoints.orders_in)?;
                    mark_ready(&endpoints.orders_in)?;
                    println!(
                        "router: discovered {} (orders_out={}, orders_in={})",
                        strategy.0,
                        orders_out.display(),
                        endpoints.orders_in.display()
                    );
                    handles.insert(strategy, StrategyHandle { reader, writer });
                }
                DiscoveryEvent::Removed { strategy } => {
                    if let Some(handle) = handles.remove(&strategy) {
                        drop(handle);
                        println!("router: removed {}", strategy.0);
                    }
                }
            }
        }

        for (strategy, handle) in handles.iter_mut() {
            while let Some(msg) = handle.reader.next()? {
                let text = std::str::from_utf8(msg.payload).unwrap_or("");
                let order_id = extract_field(text, "order_id").unwrap_or("unknown");
                let ack = format!("order_id={order_id} status=ACK");
                handle.writer.append(3, ack.as_bytes())?;
                println!("router: order from {} ({text})", strategy.0);
                handle.reader.commit()?;
                idle = false;
            }
        }

        if idle {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

struct StrategyHandle {
    reader: QueueReader,
    writer: QueueWriter,
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

fn extract_field<'a>(payload: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    payload
        .split_whitespace()
        .find_map(|part| part.strip_prefix(&prefix))
}

fn print_usage() {
    eprintln!("Usage: router [--bus-root <path>]");
}
