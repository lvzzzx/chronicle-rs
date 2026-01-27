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

Multi-Symbol Replay (Consistent Hashing):
┌─────────────────────────────────────────────────────────────────┐
│                    Message Dispatcher                           │
│                 (Main Thread Reads Queue)                       │
│                                                                 │
│  Message → Extract Symbol → hash(symbol) % N → Route to Worker │
└────────┬────────────┬────────────┬────────────┬─────────────────┘
         │            │            │            │
    ┌────▼────┐  ┌────▼────┐  ┌────▼────┐  ┌────▼────┐
    │Worker 0 │  │Worker 1 │  │Worker 2 │  │Worker N │
    │─────────│  │─────────│  │─────────│  │─────────│
    │AAPL     │  │MSFT     │  │GOOGL    │  │TSLA     │
    │IBM      │  │AMZN     │  │NVDA     │  │META     │
    │...      │  │...      │  │...      │  │...      │
    │(200     │  │(200     │  │(200     │  │(200     │
    │symbols) │  │symbols) │  │symbols) │  │symbols) │
    │         │  │         │  │         │  │         │
    │Handler  │  │Handler  │  │Handler  │  │Handler  │
    │per      │  │per      │  │per      │  │per      │
    │Symbol   │  │Symbol   │  │Symbol   │  │Symbol   │
    └─────────┘  └─────────┘  └─────────┘  └─────────┘
       │             │             │             │
       └─────────────┴─────────────┴─────────────┘
                     │
              ┌──────▼──────┐
              │ Stats/Output│
              └─────────────┘
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

For processing many symbols (e.g., 1000+) in parallel, use a **fixed worker pool** with **consistent hashing**:

**Key Design**:
- Fixed number of workers (e.g., 5 threads)
- Hash each symbol to a specific worker: `hash(symbol) % worker_count`
- All events for a symbol go to the same worker (ordering preserved)
- Each worker handles ~N/W symbols (e.g., 1000 symbols / 5 workers = 200 symbols/worker)

**Benefits**:
- Bounded concurrency (predictable resource usage)
- Per-symbol ordering guaranteed (stateful processing safe)
- No lock contention (each symbol owned by one worker)
- Lazy handler initialization (only create handlers for symbols that appear)

**Flow**:
1. Main thread reads messages from queue
2. Extract symbol from each message
3. Hash symbol → worker_id
4. Dispatch message to worker's channel
5. Worker processes message with per-symbol handler

```rust
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Parallel replay with consistent symbol-to-worker hashing.
///
/// Architecture:
/// - Fixed worker pool (e.g., 5 workers)
/// - Many symbols (e.g., 1000 symbols)
/// - Each symbol hashes to a specific worker (e.g., 200 symbols/worker)
/// - All events for a symbol are processed by the same worker (ordering preserved)
pub struct MultiSymbolReplay<H> {
    source: Box<dyn MessageSource>,
    worker_count: usize,
    handler_factory: Arc<H>,
}

/// Per-symbol event handler (user-provided logic).
pub trait SymbolHandler: Send {
    /// Process a message for this symbol.
    fn handle(&mut self, msg: &Message) -> Result<()>;

    /// Flush any buffered state.
    fn flush(&mut self) -> Result<()>;
}

impl<H> MultiSymbolReplay<H>
where
    H: Fn(&str) -> Box<dyn SymbolHandler> + Send + Sync + 'static,
{
    /// Create a multi-symbol replay with a handler factory.
    pub fn new(source: Box<dyn MessageSource>, worker_count: usize, handler_factory: H) -> Self {
        Self {
            source,
            worker_count,
            handler_factory: Arc::new(handler_factory),
        }
    }

    /// Run the replay with consistent hashing.
    pub fn run(mut self) -> Result<ReplayStats> {
        let worker_count = self.worker_count;

        // Create channels for each worker
        let mut worker_txs = Vec::new();
        let mut worker_handles = Vec::new();

        for worker_id in 0..worker_count {
            let (tx, rx) = mpsc::sync_channel::<Message>(1000);
            worker_txs.push(tx);

            let factory = Arc::clone(&self.handler_factory);
            let handle = thread::Builder::new()
                .name(format!("replay-worker-{}", worker_id))
                .spawn(move || -> Result<WorkerStats> {
                    // Per-symbol handlers (lazy-initialized)
                    let mut handlers: HashMap<String, Box<dyn SymbolHandler>> = HashMap::new();
                    let mut stats = WorkerStats::default();

                    while let Ok(msg) = rx.recv() {
                        let symbol = msg.symbol.as_deref().unwrap_or("UNKNOWN");

                        // Get or create handler for this symbol
                        let handler = handlers.entry(symbol.to_string())
                            .or_insert_with(|| factory(symbol));

                        handler.handle(&msg)?;
                        stats.messages_processed += 1;
                    }

                    // Flush all handlers
                    for (symbol, handler) in handlers.iter_mut() {
                        handler.flush()?;
                        stats.symbols_processed += 1;
                    }

                    Ok(stats)
                })?;

            worker_handles.push(handle);
        }

        // Dispatch loop: read messages and route to workers
        let mut total_messages = 0;
        let start = Instant::now();

        while let Some(msg) = self.source.next()? {
            let symbol = msg.symbol.as_deref().unwrap_or("UNKNOWN");
            let worker_id = hash_symbol_to_worker(symbol, worker_count);

            // Send to designated worker (blocks if channel full)
            worker_txs[worker_id].send(msg)?;
            total_messages += 1;

            if total_messages % 100_000 == 0 {
                let elapsed = start.elapsed().as_secs_f64();
                let throughput = total_messages as f64 / elapsed;
                println!("Dispatched {} msgs ({:.0} msg/sec)", total_messages, throughput);
            }
        }

        // Close all channels
        drop(worker_txs);

        // Wait for workers and collect stats
        let mut worker_stats = Vec::new();
        for handle in worker_handles {
            worker_stats.push(handle.join().unwrap()?);
        }

        Ok(ReplayStats {
            total_messages,
            worker_stats,
            duration: start.elapsed(),
        })
    }
}

/// Hash a symbol to a worker ID (consistent hashing).
fn hash_symbol_to_worker(symbol: &str, worker_count: usize) -> usize {
    let mut hasher = DefaultHasher::new();
    symbol.hash(&mut hasher);
    (hasher.finish() as usize) % worker_count
}

#[derive(Debug, Default)]
pub struct WorkerStats {
    pub messages_processed: u64,
    pub symbols_processed: usize,
}

#[derive(Debug)]
pub struct ReplayStats {
    pub total_messages: u64,
    pub worker_stats: Vec<WorkerStats>,
    pub duration: Duration,
}

impl ReplayStats {
    pub fn throughput(&self) -> f64 {
        self.total_messages as f64 / self.duration.as_secs_f64()
    }
}

// Usage Example: Orderbook reconstruction for 1000 symbols with 5 workers
let source = Box::new(ReplayReader::open("./queue")?);

let replay = MultiSymbolReplay::new(
    source,
    5,  // 5 workers
    |symbol| {
        // Factory creates a handler per symbol
        Box::new(OrderbookReconstructor::new(
            symbol,
            CsvSink::new(&format!("features_{}.csv", symbol)).unwrap()
        ))
    }
);

let stats = replay.run()?;
println!("Processed {} messages at {:.0} msg/sec across {} workers",
    stats.total_messages,
    stats.throughput(),
    stats.worker_stats.len()
);

// Each worker processed ~200 symbols (1000 / 5)
for (i, worker) in stats.worker_stats.iter().enumerate() {
    println!("  Worker {}: {} msgs, {} symbols",
        i,
        worker.messages_processed,
        worker.symbols_processed
    );
}
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

**Goal**: Parallel symbol replay with consistent hashing

Files to create:
- `src/replay/parallel.rs` - `MultiSymbolReplay`, `SymbolHandler` trait

**Implementation**:
- Fixed worker pool (configurable size)
- Consistent hashing: `hash(symbol) % worker_count`
- Per-worker message channels (bounded, backpressure-aware)
- Lazy per-symbol handler initialization
- Worker stats aggregation

**Tests**:
- Symbol routing (verify same symbol → same worker)
- Load distribution (verify symbols spread across workers)
- Ordering preservation (per-symbol message order maintained)
- Error propagation (worker errors surface to main thread)
- Graceful shutdown (all handlers flushed)

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
