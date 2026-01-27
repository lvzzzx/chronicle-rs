# Offline Replay System - Design Document

**Status**: Design Phase
**Version**: 0.1.0
**Date**: 2026-01-27

---

## 1. Overview

### Purpose

Enable **offline replay** of segmented log data for:
- L2/L3 orderbook reconstruction from market data
- Feature engineering and research
- High-throughput backtesting (as-fast-as-possible mode)
- Multi-symbol parallel processing

### Key Requirements

1. **Performance**: Maximum throughput, ignore original timestamps
2. **Architecture**: Separation of concerns (replay ≠ reconstruction)
3. **Filtering**: Time range, symbol, message type
4. **API**: Fluent/builder pattern with method chaining
5. **Multi-threading**: Per-symbol worker pool
6. **Output**: Write to queue, export to CSV/Parquet
7. **Abstraction**: Unified trait for live and replay readers

---

## 2. Architecture

### High-Level Design

```
┌─────────────────────────────────────────────────────────────────┐
│                         Replay Pipeline                         │
│                                                                 │
│  Source          Transform              Sink                    │
│  ┌────────┐     ┌──────────┐          ┌──────────┐            │
│  │ Replay │────►│ Filters  │─────────►│ Queue    │            │
│  │ Reader │     │ - Time   │          │ Writer   │            │
│  └────────┘     │ - Symbol │          └──────────┘            │
│                 │ - Type   │                                   │
│                 └──────────┘          ┌──────────┐            │
│                       │               │   CSV    │            │
│                       └──────────────►│ Exporter │            │
│                                       └──────────┘            │
└─────────────────────────────────────────────────────────────────┘

Multi-Symbol Replay:
┌─────────────────────────────────────────────────────────────────┐
│                       Worker Pool                               │
│                                                                 │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐        │
│  │  Worker 1   │    │  Worker 2   │    │  Worker N   │        │
│  │  Symbol: A  │    │  Symbol: B  │    │  Symbol: C  │        │
│  │             │    │             │    │             │        │
│  │  Replay ──► │    │  Replay ──► │    │  Replay ──► │        │
│  │  Filter     │    │  Filter     │    │  Filter     │        │
│  │  Orderbook  │    │  Orderbook  │    │  Orderbook  │        │
│  │  Features   │    │  Features   │    │  Features   │        │
│  │  Sink       │    │  Sink       │    │  Sink       │        │
│  └─────────────┘    └─────────────┘    └─────────────┘        │
└─────────────────────────────────────────────────────────────────┘
```

### Component Layers

1. **Source Layer**: `MessageSource` trait
   - `QueueReader` (live)
   - `ReplayReader` (offline)
   - Future: network streams, synthetic data generators

2. **Transform Layer**: Composable operators
   - `Filter`: Time range, symbol, type
   - `Map`: Transform messages
   - `Sample`: Downsample output
   - `Batch`: Group messages

3. **Sink Layer**: Output destinations
   - `QueueSink`: Write to Chronicle queue
   - `CsvSink`: Export to CSV
   - `ParquetSink`: Export to Parquet (future)
   - `ConsoleSink`: Debug output

---

## 3. Trait Definitions

### 3.1 MessageSource

Unified abstraction for live and replay readers.

```rust
/// A source of messages that can be consumed sequentially.
pub trait MessageSource {
    /// Returns the next message, or None if exhausted.
    fn next(&mut self) -> Result<Option<Message>>;

    /// Optional: seek to a specific position (for replay readers).
    fn seek_to_seq(&mut self, seq: u64) -> Result<bool> {
        Err(Error::Unsupported("seek not supported"))
    }

    /// Optional: seek to a timestamp (for replay readers).
    fn seek_to_timestamp(&mut self, timestamp_ns: u64) -> Result<bool> {
        Err(Error::Unsupported("seek not supported"))
    }

    /// Returns an iterator over messages (default implementation).
    fn iter(&mut self) -> MessageIterator<'_, Self>
    where
        Self: Sized,
    {
        MessageIterator { source: self }
    }
}

/// Owned message that can outlive the reader.
#[derive(Debug, Clone)]
pub struct Message {
    pub seq: u64,
    pub timestamp_ns: u64,
    pub type_id: u16,
    pub symbol: Option<String>,  // Decoded from payload
    pub payload: Vec<u8>,
}

impl Message {
    /// Parse from MessageView (zero-copy → owned).
    pub fn from_view(view: &MessageView<'_>) -> Self {
        Self {
            seq: view.seq,
            timestamp_ns: view.timestamp_ns,
            type_id: view.type_id,
            symbol: extract_symbol(view.payload),  // Application-specific
            payload: view.payload.to_vec(),
        }
    }
}
```

### 3.2 Transform Trait

```rust
/// A transformation stage in the pipeline.
pub trait Transform {
    /// Transform a message, returning None to filter it out.
    fn transform(&mut self, msg: Message) -> Result<Option<Message>>;
}

/// Built-in transforms

/// Filters messages by time range.
pub struct TimeRangeFilter {
    start_ns: u64,
    end_ns: u64,
}

impl Transform for TimeRangeFilter {
    fn transform(&mut self, msg: Message) -> Result<Option<Message>> {
        if msg.timestamp_ns >= self.start_ns && msg.timestamp_ns <= self.end_ns {
            Ok(Some(msg))
        } else {
            Ok(None)
        }
    }
}

/// Filters messages by symbol.
pub struct SymbolFilter {
    symbols: HashSet<String>,
}

impl Transform for SymbolFilter {
    fn transform(&mut self, msg: Message) -> Result<Option<Message>> {
        if let Some(symbol) = &msg.symbol {
            if self.symbols.contains(symbol) {
                return Ok(Some(msg));
            }
        }
        Ok(None)
    }
}

/// Filters messages by type ID.
pub struct TypeFilter {
    types: HashSet<u16>,
}

impl Transform for TypeFilter {
    fn transform(&mut self, msg: Message) -> Result<Option<Message>> {
        if self.types.contains(&msg.type_id) {
            Ok(Some(msg))
        } else {
            Ok(None)
        }
    }
}

/// Samples every Nth message.
pub struct Sampler {
    interval: usize,
    count: usize,
}

impl Transform for Sampler {
    fn transform(&mut self, msg: Message) -> Result<Option<Message>> {
        self.count += 1;
        if self.count % self.interval == 0 {
            Ok(Some(msg))
        } else {
            Ok(None)
        }
    }
}
```

### 3.3 Sink Trait

```rust
/// A destination for processed messages.
pub trait Sink {
    /// Write a message to the sink.
    fn write(&mut self, msg: &Message) -> Result<()>;

    /// Flush any buffered data.
    fn flush(&mut self) -> Result<()>;
}

/// Writes messages to a Chronicle queue.
pub struct QueueSink {
    writer: QueueWriter,
}

impl Sink for QueueSink {
    fn write(&mut self, msg: &Message) -> Result<()> {
        self.writer.append(msg.type_id, &msg.payload)?;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.writer.flush_sync()
    }
}

/// Writes messages to CSV.
pub struct CsvSink {
    writer: csv::Writer<File>,
}

impl Sink for CsvSink {
    fn write(&mut self, msg: &Message) -> Result<()> {
        self.writer.write_record(&[
            msg.seq.to_string(),
            msg.timestamp_ns.to_string(),
            msg.type_id.to_string(),
            msg.symbol.as_deref().unwrap_or(""),
            // Format payload as hex or decode application-specific
        ])?;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.writer.flush()?;
        Ok(())
    }
}
```

---

## 4. Fluent Pipeline API

### 4.1 ReplayPipeline Builder

```rust
/// Fluent builder for constructing replay pipelines.
pub struct ReplayPipeline<S: MessageSource> {
    source: S,
    transforms: Vec<Box<dyn Transform>>,
    sinks: Vec<Box<dyn Sink>>,
}

impl<S: MessageSource> ReplayPipeline<S> {
    /// Create a pipeline from a source.
    pub fn from_source(source: S) -> Self {
        Self {
            source,
            transforms: Vec::new(),
            sinks: Vec::new(),
        }
    }

    /// Add a time range filter.
    pub fn filter_time_range(mut self, start_ns: u64, end_ns: u64) -> Self {
        self.transforms.push(Box::new(TimeRangeFilter {
            start_ns,
            end_ns,
        }));
        self
    }

    /// Add a symbol filter.
    pub fn filter_symbols(mut self, symbols: &[&str]) -> Self {
        let symbols = symbols.iter().map(|s| s.to_string()).collect();
        self.transforms.push(Box::new(SymbolFilter { symbols }));
        self
    }

    /// Add a type ID filter.
    pub fn filter_types(mut self, types: &[u16]) -> Self {
        let types = types.iter().copied().collect();
        self.transforms.push(Box::new(TypeFilter { types }));
        self
    }

    /// Sample every Nth message.
    pub fn sample(mut self, interval: usize) -> Self {
        self.transforms.push(Box::new(Sampler {
            interval,
            count: 0,
        }));
        self
    }

    /// Add a custom transform.
    pub fn transform<T: Transform + 'static>(mut self, transform: T) -> Self {
        self.transforms.push(Box::new(transform));
        self
    }

    /// Write to a Chronicle queue.
    pub fn write_to_queue(mut self, path: impl AsRef<Path>) -> Result<Self> {
        let writer = Queue::open_publisher(path)?;
        self.sinks.push(Box::new(QueueSink { writer }));
        Ok(self)
    }

    /// Write to a CSV file.
    pub fn write_to_csv(mut self, path: impl AsRef<Path>) -> Result<Self> {
        let file = File::create(path)?;
        let writer = csv::Writer::from_writer(file);
        self.sinks.push(Box::new(CsvSink { writer }));
        Ok(self)
    }

    /// Add a custom sink.
    pub fn sink<K: Sink + 'static>(mut self, sink: K) -> Self {
        self.sinks.push(Box::new(sink));
        self
    }

    /// Execute the pipeline.
    pub fn run(mut self) -> Result<PipelineStats> {
        let mut stats = PipelineStats::default();
        let start = Instant::now();

        while let Some(msg) = self.source.next()? {
            stats.messages_read += 1;

            // Apply transforms
            let mut msg = Some(msg);
            for transform in &mut self.transforms {
                if let Some(m) = msg {
                    msg = transform.transform(m)?;
                    if msg.is_none() {
                        stats.messages_filtered += 1;
                        break;
                    }
                } else {
                    break;
                }
            }

            // Write to sinks
            if let Some(msg) = msg {
                for sink in &mut self.sinks {
                    sink.write(&msg)?;
                }
                stats.messages_written += 1;
            }
        }

        // Flush all sinks
        for sink in &mut self.sinks {
            sink.flush()?;
        }

        stats.duration = start.elapsed();
        Ok(stats)
    }
}

#[derive(Debug, Default)]
pub struct PipelineStats {
    pub messages_read: u64,
    pub messages_filtered: u64,
    pub messages_written: u64,
    pub duration: Duration,
}

impl PipelineStats {
    pub fn throughput(&self) -> f64 {
        self.messages_read as f64 / self.duration.as_secs_f64()
    }
}
```

### 4.2 Usage Examples

```rust
// Example 1: Simple replay with filtering
let reader = ReplayReader::open("./queue")?;
let stats = ReplayPipeline::from_source(reader)
    .filter_symbols(&["AAPL", "MSFT"])
    .filter_time_range(start_ts, end_ts)
    .write_to_csv("output.csv")?
    .run()?;

println!("Processed {} msgs at {:.2} msg/sec",
    stats.messages_read,
    stats.throughput()
);

// Example 2: Multi-output pipeline
let reader = ReplayReader::open("./queue")?;
ReplayPipeline::from_source(reader)
    .filter_types(&[TYPE_TRADE, TYPE_QUOTE])
    .write_to_queue("./processed")?
    .write_to_csv("export.csv")?
    .run()?;

// Example 3: Downsampling
let reader = ReplayReader::open("./queue")?;
ReplayPipeline::from_source(reader)
    .filter_symbols(&["BTC-USD"])
    .sample(100)  // Every 100th message
    .write_to_csv("sampled.csv")?
    .run()?;

// Example 4: Custom transform
struct OrderbookReconstructor {
    book: L2Book,
}

impl Transform for OrderbookReconstructor {
    fn transform(&mut self, msg: Message) -> Result<Option<Message>> {
        // Update internal orderbook state
        self.book.apply(&msg)?;

        // Emit snapshot every 1000 messages
        if msg.seq % 1000 == 0 {
            let snapshot = self.book.snapshot();
            Ok(Some(Message::from_snapshot(snapshot)))
        } else {
            Ok(None)  // Filter out intermediate updates
        }
    }
}

let reader = ReplayReader::open("./queue")?;
ReplayPipeline::from_source(reader)
    .transform(OrderbookReconstructor { book: L2Book::new() })
    .write_to_queue("./snapshots")?
    .run()?;
```

---

## 5. Multi-Symbol Worker Pool

### Architecture

```rust
/// Parallel replay with per-symbol workers.
pub struct MultiSymbolReplay {
    base_path: PathBuf,
    symbols: Vec<String>,
    worker_count: usize,
}

impl MultiSymbolReplay {
    /// Create a multi-symbol replay configuration.
    pub fn new(base_path: impl AsRef<Path>) -> Self {
        Self {
            base_path: base_path.as_ref().to_path_buf(),
            symbols: Vec::new(),
            worker_count: num_cpus::get(),
        }
    }

    /// Add symbols to replay.
    pub fn add_symbols(mut self, symbols: &[&str]) -> Self {
        self.symbols.extend(symbols.iter().map(|s| s.to_string()));
        self
    }

    /// Set worker thread count (default: num_cpus).
    pub fn worker_count(mut self, count: usize) -> Self {
        self.worker_count = count;
        self
    }

    /// Execute replay with a per-symbol pipeline factory.
    pub fn run<F>(self, pipeline_factory: F) -> Result<Vec<PipelineStats>>
    where
        F: Fn(String) -> Result<ReplayPipeline<ReplayReader>> + Send + Sync + Clone + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let symbols = Arc::new(self.symbols);
        let factory = Arc::new(pipeline_factory);

        // Spawn worker threads
        let mut handles = Vec::new();
        for worker_id in 0..self.worker_count {
            let symbols = Arc::clone(&symbols);
            let factory = Arc::clone(&factory);
            let tx = tx.clone();

            let handle = thread::spawn(move || {
                // Assign symbols to this worker (round-robin)
                for (i, symbol) in symbols.iter().enumerate() {
                    if i % self.worker_count == worker_id {
                        let pipeline = factory(symbol.clone())?;
                        let stats = pipeline.run()?;
                        tx.send((symbol.clone(), stats))?;
                    }
                }
                Ok::<_, Error>(())
            });
            handles.push(handle);
        }

        drop(tx);  // Close sender

        // Collect results
        let mut results = Vec::new();
        while let Ok((symbol, stats)) = rx.recv() {
            println!("[{}] {} msgs at {:.2} msg/sec",
                symbol,
                stats.messages_read,
                stats.throughput()
            );
            results.push(stats);
        }

        // Wait for workers
        for handle in handles {
            handle.join().unwrap()?;
        }

        Ok(results)
    }
}

// Usage
let replay = MultiSymbolReplay::new("./queue")
    .add_symbols(&["AAPL", "MSFT", "GOOGL", "AMZN"])
    .worker_count(4);

replay.run(|symbol| {
    let reader = ReplayReader::open_with_filter("./queue", &symbol)?;
    Ok(ReplayPipeline::from_source(reader)
        .filter_time_range(start, end)
        .transform(OrderbookReconstructor::new())
        .write_to_csv(&format!("features_{}.csv", symbol))?)
})?;
```

---

## 6. Implementation Plan

### Phase 1: Core Traits & Reader (Week 1)

**Goal**: Basic replay capability

Files to create:
- `src/replay/mod.rs` - Module definition
- `src/replay/source.rs` - `MessageSource` trait
- `src/replay/message.rs` - `Message` type
- `src/replay/reader.rs` - `ReplayReader` implementation

```rust
// src/replay/reader.rs (stub)
pub struct ReplayReader {
    inner: QueueReader,
    // Additional replay-specific state
}

impl MessageSource for ReplayReader {
    fn next(&mut self) -> Result<Option<Message>> {
        match self.inner.next()? {
            Some(view) => Ok(Some(Message::from_view(&view))),
            None => Ok(None),
        }
    }

    fn seek_to_seq(&mut self, seq: u64) -> Result<bool> {
        self.inner.seek_seq(seq)
    }

    fn seek_to_timestamp(&mut self, timestamp_ns: u64) -> Result<bool> {
        self.inner.seek_timestamp(timestamp_ns)
    }
}

impl MessageSource for QueueReader {
    fn next(&mut self) -> Result<Option<Message>> {
        match self.next()? {
            Some(view) => Ok(Some(Message::from_view(&view))),
            None => Ok(None),
        }
    }
}
```

**Tests**:
- Open replay reader from existing queue
- Iterate through messages
- Seek to timestamp/sequence
- Compare live vs replay reader

---

### Phase 2: Transforms & Filters (Week 2)

**Goal**: Composable transformations

Files to create:
- `src/replay/transform.rs` - `Transform` trait
- `src/replay/filters.rs` - Built-in filters

**Tests**:
- Time range filtering
- Symbol filtering
- Type filtering
- Sampling
- Custom transforms

---

### Phase 3: Pipeline & Sinks (Week 3)

**Goal**: Fluent API and output

Files to create:
- `src/replay/pipeline.rs` - `ReplayPipeline` builder
- `src/replay/sink.rs` - `Sink` trait
- `src/replay/sinks/queue.rs` - QueueSink
- `src/replay/sinks/csv.rs` - CsvSink

**Tests**:
- Build and execute pipelines
- Multiple sinks
- Statistics collection
- Error handling

---

### Phase 4: Multi-Threading (Week 4)

**Goal**: Parallel symbol replay

Files to create:
- `src/replay/parallel.rs` - `MultiSymbolReplay`

**Tests**:
- Per-symbol workers
- Result aggregation
- Load balancing
- Error propagation

---

### Phase 5: Orderbook Integration (Week 5)

**Goal**: Demonstrate use case

Files to create:
- `src/replay/orderbook.rs` - Orderbook reconstruction transform

**Tests**:
- L2 orderbook reconstruction
- Feature derivation
- Snapshot generation

---

## 7. Performance Targets

| Metric | Target | Notes |
|--------|--------|-------|
| Replay throughput | >1M msg/sec | Single thread, no transforms |
| Replay throughput | >500K msg/sec | With filtering + CSV output |
| Multi-symbol throughput | >5M msg/sec | 8 cores, 8 symbols |
| Memory overhead | <100MB | Per worker, excluding orderbook |
| Startup latency | <100ms | Open reader + seek to start |

---

## 8. Open Questions

1. **Symbol Extraction**: How to extract symbol from payload?
   - Option A: Application-specific callback
   - Option B: Standard field in message header
   - Option C: Parse based on type_id

2. **Backpressure**: How to handle slow sinks?
   - Option A: Blocking write (simple)
   - Option B: Async with buffering (complex)
   - Option C: Fail-fast (current)

3. **Progress Tracking**: How to report progress during replay?
   - Option A: Callback every N messages
   - Option B: Shared atomic counter
   - Option C: Channel-based updates

4. **Error Handling**: Continue or abort on transform errors?
   - Option A: Abort on first error (fail-fast)
   - Option B: Skip and log errors
   - Option C: Configurable strategy

---

## 9. Future Enhancements

### 9.1 Snapshotting
- Save orderbook state periodically
- Resume replay from snapshot + delta
- Trade-off: memory vs. startup time

### 9.2 Async Pipeline
- Non-blocking transforms
- Parallel stage execution
- Requires tokio/async-std

### 9.3 Distributed Replay
- Replay across multiple machines
- Partition by symbol
- Aggregate results

### 9.4 Real-Time Hybrid
- Start with replay, transition to live
- Seamless handoff at current timestamp
- Useful for continuous backtesting

---

## References

- **Existing Code**: `src/core/reader.rs` (QueueReader API)
- **Seek Implementation**: `src/core/seek_index.rs`
- **Storage Format**: `docs/CORE_MODULE_ARCHITECTURE.md`
- **API Guidelines**: `docs/LLM_FRIENDLY_CODING_STANDARDS.md`

---

**Next Steps**:
1. Review this design with team
2. Create stub implementations (Phase 1)
3. Write integration tests
4. Iterate based on performance benchmarks

**Maintainer**: Chronicle-RS Team
**Last Updated**: 2026-01-27
