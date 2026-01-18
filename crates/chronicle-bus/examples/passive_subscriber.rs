use std::time::Duration;

use chronicle_bus::{PassiveConfig, PassiveDiscovery, PassiveEvent};
use chronicle_core::{QueueReader, WaitStrategy};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let queue_path = args
        .next()
        .expect("usage: passive_subscriber <queue_path> [reader_name]");
    let reader_name = args.next().unwrap_or_else(|| "subscriber".to_string());

    let mut discovery = PassiveDiscovery::new(queue_path, reader_name, PassiveConfig::default());
    let mut reader: Option<QueueReader> = None;

    loop {
        for event in discovery.poll(reader.as_ref())? {
            match event {
                PassiveEvent::Connected(mut new_reader) => {
                    new_reader.set_wait_strategy(WaitStrategy::SpinThenPark { spin_us: 10 });
                    reader = Some(new_reader);
                }
                PassiveEvent::Disconnected(reason) => {
                    eprintln!("reader disconnected: {reason:?}");
                    reader = None;
                }
                PassiveEvent::Waiting => {}
            }
        }

        if let Some(active) = reader.as_mut() {
            active.wait(Some(Duration::from_millis(1)))?;
            while let Some(_msg) = active.next()? {
                // Process messages here.
            }
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
