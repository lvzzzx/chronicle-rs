use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chronicle_core::{Queue, Clock, QuantaClock, ReaderConfig, StartMode};
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
    // Open subscriber
    let mut reader = Queue::open_subscriber_with_config(
        &args.path,
        "monitor",
        ReaderConfig {
            start_mode: StartMode::ResumeLatest,
            ..Default::default()
        },
    )?;
    
    // Jump to end - we only care about new data (latency of old data is misleading/huge)
    // Actually, "open_subscriber" defaults to start (0). We should seek to end?
    // "QueueReader" implementation details needed. Usually open_subscriber starts at 0.
    // For monitoring "live", we might want to skip old data or just crunch through it fast.
    // Let's crunch through it, but maybe not record stats for the backlog?
    // For now, let's just run. If the queue is huge, it will take time to catch up.
    // Ideally, `Queue::open_tail` or similar would be good, but `QueueReader` handles persistence.
    // We can just loop until `next()` returns None (caught up) before starting stats?
    // Let's just process normally.

    let clock = QuantaClock::new();
    let mut histogram = Histogram::<u64>::new(3)?;
    let mut count_window = 0u64;
    let mut last_tick = Instant::now();
    let interval = Duration::from_millis(args.interval);

    loop {
        // 1. Drain Queue
        // We limit burst to avoid freezing the UI if queue is huge
        let mut burst = 0;
        while burst < 10_000 {
            match reader.next() {
                Ok(Some(msg)) => {
                    let now = clock.now();
                    // Saturating sub handles slight clock skew or unsynchronized starts
                    let latency = now.saturating_sub(msg.timestamp_ns);
                    let _ = histogram.record(latency);
                    count_window += 1;
                    reader.commit()?;
                    burst += 1;
                }
                Ok(None) => break,
                Err(e) => return Err(e.into()),
            }
        }

        // 2. Render
        if last_tick.elapsed() >= interval {
            // Calculate stats
            let rate = count_window as f64 / last_tick.elapsed().as_secs_f64();
            let last_stats = LatencyStats {
                p50: histogram.value_at_quantile(0.5),
                p99: histogram.value_at_quantile(0.99),
                p999: histogram.value_at_quantile(0.999),
                max: histogram.max(),
                rate,
            };

            terminal.draw(|f| ui(f, &args, &last_stats))?;

            // Reset for next window
            histogram.reset();
            count_window = 0;
            last_tick = Instant::now();
        }

        // 3. Handle Input
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
}

fn ui(f: &mut Frame, args: &MonitorArgs, stats: &LatencyStats) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(f.size());

    let title = Paragraph::new(format!("Chronicle Monitor: {}", args.path.display()))
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, chunks[0]);

    let stats_text = vec![
        Line::from(vec![
            Span::raw("Rate: "),
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