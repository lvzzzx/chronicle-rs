use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chronicle_bus::{SubscriberConfig, SubscriberDiscovery, SubscriberEvent};
use chronicle_core::{Clock, QuantaClock, QueueReader, StartMode};
use clap::Args;
use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use hdrhistogram::Histogram;
use ratatui::prelude::*;
use ratatui::widgets::*;

#[derive(Args, Debug)]
pub struct MonitorArgs {
    /// Path to the queue to monitor
    #[arg(name = "QUEUE")]
    path: PathBuf,

    /// Update interval in milliseconds
    #[arg(long, default_value_t = 100)]
    interval: u64,
}

pub fn run(args: MonitorArgs) -> Result<(), Box<dyn std::error::Error>> {
    // Setup Terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run_monitor(&mut terminal, args);

    // Restore Terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

fn run_monitor<B: Backend>(terminal: &mut Terminal<B>, args: MonitorArgs) -> Result<(), Box<dyn std::error::Error>> {
    // We need to manage the connection state
    let mut reader: Option<QueueReader> = None;
    let connect_interval = Duration::from_millis(500);
    let mut discovery = SubscriberDiscovery::new(
        args.path.clone(),
        "monitor",
        SubscriberConfig {
            poll_interval: connect_interval,
            start_mode: StartMode::ResumeLatest,
            ..SubscriberConfig::default()
        },
    );
    let mut connection_status = "Initializing...".to_string();

    let clock = QuantaClock::new();
    let mut histogram = Histogram::<u64>::new(3)?;
    let mut count_window = 0u64;
    let mut last_tick = Instant::now();
    let interval = Duration::from_millis(args.interval);

    loop {
        // 1. Connection Management
        match discovery.poll(reader.as_ref()) {
            Ok(events) => {
                for event in events {
                    match event {
                        SubscriberEvent::Connected(r) => {
                            reader = Some(r);
                            connection_status = "Connected".to_string();
                            // Reset stats on new connection
                            histogram.reset();
                            count_window = 0;
                            last_tick = Instant::now();
                        }
                        SubscriberEvent::Disconnected(reason) => {
                            reader = None;
                            connection_status = format!("Disconnected: {reason:?}");
                        }
                        SubscriberEvent::Waiting => {
                            if reader.is_none() {
                                connection_status = "Waiting for queue".to_string();
                            }
                        }
                    }
                }
            }
            Err(e) => {
                connection_status = format!("Discovery error: {}", e);
            }
        }

        // 2. Drain Queue (if connected)
        let mut should_disconnect = false;
        if let Some(r) = &mut reader {
            // We limit burst to avoid freezing the UI if queue is huge
            let mut burst = 0;
            while burst < 10_000 {
                match r.next() {
                    Ok(Some(msg)) => {
                        let now = clock.now();
                        // Saturating sub handles slight clock skew or unsynchronized starts
                        let latency = now.saturating_sub(msg.timestamp_ns);
                        let _ = histogram.record(latency);
                        count_window += 1;
                        burst += 1;
                    }
                    Ok(None) => break,
                    Err(e) => {
                        // If we lose connection or hit a fatal error, drop the reader and go back to waiting
                        connection_status = format!("Error reading queue: {}", e);
                        should_disconnect = true;
                        break;
                    }
                }
            }
            // Commit progress after the burst
            if !should_disconnect {
                if let Err(e) = r.commit() {
                     connection_status = format!("Error committing offset: {}", e);
                     // Failure to commit usually implies FS issues, maybe should detach?
                     // For now, we log it in status but keep reading.
                }
            }
        }
        
        if should_disconnect {
            reader = None;
        }

        // 3. Render
        if last_tick.elapsed() >= interval {
            // Calculate stats
            let rate = if count_window > 0 {
                count_window as f64 / last_tick.elapsed().as_secs_f64()
            } else {
                0.0
            };
            
            let last_stats = if reader.is_some() {
                 LatencyStats {
                    p50: histogram.value_at_quantile(0.5),
                    p99: histogram.value_at_quantile(0.99),
                    p999: histogram.value_at_quantile(0.999),
                    max: histogram.max(),
                    rate,
                    connected: true,
                    status: connection_status.clone(),
                }
            } else {
                LatencyStats {
                    connected: false,
                    status: connection_status.clone(),
                    ..Default::default()
                }
            };

            terminal.draw(|f| ui(f, &args, &last_stats))?;

            // Reset for next window
            if reader.is_some() {
                histogram.reset();
                count_window = 0;
                last_tick = Instant::now();
            }
        }

        // 4. Handle Input
        if event::poll(Duration::from_millis(1))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q') {
                    return Ok(());
                }
            }
        }
    }
}

#[derive(Default)]
struct LatencyStats {
    p50: u64,
    p99: u64,
    p999: u64,
    max: u64,
    rate: f64,
    connected: bool,
    status: String,
}

fn ui(f: &mut Frame, args: &MonitorArgs, stats: &LatencyStats) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(f.size());

    let title_color = if stats.connected { Color::Green } else { Color::Yellow };
    let title = Paragraph::new(format!("Chronicle Monitor: {}", args.path.display()))
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(title_color)));
    f.render_widget(title, chunks[0]);

    if !stats.connected {
        let status = Paragraph::new(format!("Status: {}", stats.status))
             .style(Style::default().fg(Color::Yellow))
             .block(Block::default().title("Status").borders(Borders::ALL));
        f.render_widget(status, chunks[1]);
        return;
    }

    let stats_text = vec![
        Line::from(vec![
            Span::raw("Status: "),
            Span::styled("Connected", Style::default().fg(Color::Green)),
        ]),
        Line::from(vec![
            Span::raw("Rate:   "),
            Span::styled(format!("{:.0} msg/s", stats.rate), Style::default().fg(Color::Cyan)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("Latencies (ns):"),
        ]),
        Line::from(vec![
            Span::raw("  p50:   "),
            Span::styled(format!("{}", stats.p50), Style::default().fg(Color::Green)),
        ]),
        Line::from(vec![
            Span::raw("  p99:   "),
            Span::styled(format!("{}", stats.p99), Style::default().fg(Color::Yellow)),
        ]),
        Line::from(vec![
            Span::raw("  p99.9: "),
            Span::styled(format!("{}", stats.p999), Style::default().fg(Color::Red)),
        ]),
        Line::from(vec![
            Span::raw("  Max:   "),
            Span::styled(format!("{}", stats.max), Style::default().fg(Color::Magenta)),
        ]),
    ];

    let body = Paragraph::new(stats_text)
        .block(Block::default().title("Statistics").borders(Borders::ALL));
    f.render_widget(body, chunks[1]);
}
