# LLM-Friendly Coding Standards

**Status**: Draft
**Version**: 0.1.0
**Last Updated**: 2026-01-27

## Philosophy

In the era of AI-assisted development, code must be optimized for **collaborative reasoning** between humans and LLMs. These standards ensure that AI tools can:

1. Understand intent quickly from minimal context
2. Reason about correctness and trade-offs
3. Generate safe, idiomatic modifications
4. Navigate large codebases efficiently

**Core Principle**: Maximize information density while minimizing cognitive overhead.

---

## 1. Explicit State Over Magic

### ❌ Before: Hidden Constants
```rust
const READER_TTL_NS: u64 = 30_000_000_000;
const MAX_RETENTION_LAG: u64 = 10 * 1024 * 1024 * 1024;

fn cleanup_segments(root: &Path, segment: u64, offset: u64) -> Result<()> {
    // Uses globals implicitly
}
```

### ✅ After: Structured Configuration
```rust
/// Configuration for segment retention and cleanup.
#[derive(Debug, Clone, Copy)]
pub struct RetentionConfig {
    /// Time-to-live for reader heartbeats.
    pub reader_ttl: Duration,
    /// Maximum lag (in bytes) before readers are ignored.
    pub max_reader_lag: u64,
}

fn cleanup_segments(
    root: &Path,
    segment: u64,
    offset: u64,
    config: &RetentionConfig,
) -> Result<()> {
    // Behavior is explicit and configurable
}
```

**Why This Helps**:
- LLM can suggest `config.reader_ttl = Duration::from_secs(60)` without hunting for constants
- Configuration becomes searchable and discoverable
- Tests can easily override behavior

---

## 2. Version-Aware Data Structures

### ❌ Before: Implicit Format
```rust
#[repr(C)]
pub struct MessageHeader {
    pub commit_len: u32,
    pub _pad0: u32,  // Wasted space
    pub seq: u64,
    // ...
}
```

### ✅ After: Explicit Versioning
```rust
pub const MSG_VERSION: u8 = 1;

#[repr(C, align(64))]
pub struct MessageHeader {
    pub commit_len: u32,
    pub version: u8,      // Format version for evolution
    pub _pad0: [u8; 3],
    pub seq: u64,
    // ...
}

impl MessageHeader {
    pub fn from_bytes(bytes: &[u8; 64]) -> Result<Self> {
        // ...
        if version != MSG_VERSION {
            return Err(Error::UnsupportedVersion(version as u32));
        }
        // ...
    }
}
```

**Why This Helps**:
- LLM can reason about compatibility when suggesting changes
- Migration paths become explicit
- Future-proofing is visible in the type system

---

## 3. Observability by Design

### ❌ Before: Silent Failures
```rust
struct RetentionWorker {
    request_tx: mpsc::SyncSender<(u64, u64)>,
}

impl RetentionWorker {
    fn new(...) -> Result<Self> {
        thread::spawn(move || {
            // Errors swallowed silently
            let _ = cleanup_segments(...);
        });
        Ok(Self { request_tx })
    }
}
```

### ✅ After: Metrics Exposed
```rust
struct RetentionWorker {
    request_tx: mpsc::SyncSender<(u64, u64)>,
    errors: Arc<AtomicU64>,  // Track failures
}

#[derive(Debug, Clone, Default)]
pub struct WriterMetrics {
    pub prealloc_errors: u64,
    pub seal_errors: u64,
    pub retention_errors: u64,
    pub rolls: u64,
    pub bytes_written: u64,
}

impl QueueWriter {
    pub fn metrics(&self) -> WriterMetrics {
        WriterMetrics {
            retention_errors: self.retention_worker.error_count(),
            // ...
        }
    }
}
```

**Why This Helps**:
- LLM can suggest monitoring integration: `if writer.metrics().seal_errors > 0 { alert!(...) }`
- Debugging becomes data-driven
- Performance issues are visible

---

## 4. Progressive API Complexity

### ❌ Before: One-Size-Fits-All
```rust
impl QueueReader {
    pub fn next(&mut self) -> Result<Option<MessageView<'_>>> {
        // Only option for consuming messages
    }
}
```

### ✅ After: Layered Abstractions
```rust
impl QueueReader {
    /// Read single message (simple, zero-copy).
    pub fn next(&mut self) -> Result<Option<MessageView<'_>>> { /* ... */ }

    /// Bulk read into Vec (allocates, convenient).
    pub fn drain_available(&mut self, max: usize) -> Result<Vec<OwnedMessage>> { /* ... */ }

    /// Callback processing (zero-copy, maximum control).
    pub fn for_each_available<F>(&mut self, max: usize, f: F) -> Result<usize>
    where F: FnMut(MessageView<'_>) -> bool
    { /* ... */ }
}
```

**Why This Helps**:
- LLM can suggest appropriate level: "For throughput, use `drain_available(1000)`"
- Patterns (`drain_X`, `for_each_X`) generalize across the codebase
- Beginners start simple, experts optimize later

---

## 5. Self-Documenting Type Names

### ❌ Before: Opaque Tuples
```rust
fn min_live_reader_position(
    root: &Path,
    head_segment: u64,
    head_offset: u64,
    segment_size: u64,
) -> Result<u64> { /* ... */ }

// What does the u64 return value mean?
```

### ✅ After: Named Types
```rust
#[derive(Debug, Clone, Copy)]
pub struct GlobalPosition(pub u64);

#[derive(Debug, Clone, Copy)]
pub struct SegmentId(pub u64);

fn min_live_reader_position(
    root: &Path,
    head: GlobalPosition,
    segment_size: u64,
    config: &RetentionConfig,
) -> Result<GlobalPosition> { /* ... */ }
```

**Why This Helps**:
- Type errors catch bugs: `SegmentId` can't be confused with `GlobalPosition`
- LLM understands semantic meaning, not just syntax
- Refactoring is safer

---

## 6. Rich, Structured Errors

### ❌ Before: Generic Errors
```rust
pub enum Error {
    Io(std::io::Error),
    Failed,  // What failed?
}

// Elsewhere:
if some_check() {
    return Err(Error::Failed);
}
```

### ✅ After: Contextual Errors
```rust
pub enum Error {
    Io(std::io::Error),
    Corrupt(&'static str),           // What's corrupt
    UnsupportedVersion(u32),         // Which version
    PayloadTooLarge,                 // Clear constraint
    QueueFull,                       // Actionable
    WriterAlreadyActive,             // State conflict
}

// Elsewhere:
if header.version != MSG_VERSION {
    return Err(Error::UnsupportedVersion(header.version as u32));
}
```

**Why This Helps**:
- LLM can suggest recovery: `match e { Error::QueueFull => backpressure(), ... }`
- Error messages guide debugging
- Pattern matching becomes meaningful

---

## 7. Module-Sized Functions

### ❌ Before: God Function
```rust
pub fn process_orders(state: &mut State, orders: &[Order]) -> Result<()> {
    // 500 lines of validation, business logic, side effects
}
```

### ✅ After: Composable Steps
```rust
pub fn process_orders(state: &mut State, orders: &[Order]) -> Result<()> {
    let validated = validate_orders(orders)?;
    let grouped = group_by_symbol(&validated);
    for (symbol, batch) in grouped {
        apply_batch(state, symbol, batch)?;
    }
    Ok(())
}

fn validate_orders(orders: &[Order]) -> Result<Vec<ValidatedOrder>> {
    // 30 lines: focused, testable
}

fn group_by_symbol(orders: &[ValidatedOrder]) -> HashMap<Symbol, Vec<ValidatedOrder>> {
    // 15 lines: pure logic
}

fn apply_batch(state: &mut State, symbol: Symbol, batch: Vec<ValidatedOrder>) -> Result<()> {
    // 40 lines: state mutation isolated
}
```

**Why This Helps**:
- LLM can reason about each function independently
- Easier to unit test and mock
- Function names document the algorithm

---

## 8. Documented Trade-Offs

### ❌ Before: Silent Choices
```rust
pub struct WriterConfig {
    pub defer_seal_sync: bool,  // What does this do?
}
```

### ✅ After: Explicit Consequences
```rust
pub struct WriterConfig {
    /// Offload segment sync to background thread (lower latency, not durable to power loss).
    ///
    /// **Trade-off**: P99 latency improves by 10-100x, but unflushed data may be lost
    /// if the process crashes before background sync completes.
    ///
    /// **Use case**: Market data capture where some loss is acceptable.
    pub defer_seal_sync: bool,

    // ...
}
```

**Why This Helps**:
- LLM can suggest: "For HFT, enable `defer_seal_sync`. For ledger, disable it."
- Architects understand implications from code alone
- Trade-offs are searchable

---

## 9. Consistent Naming Patterns

### Pattern Families

**Discovery**: `list_X`, `find_X`, `load_X`
```rust
fn list_segments(root: &Path) -> Result<Vec<u64>>
fn find_oldest_segment(root: &Path) -> Result<u64>
fn load_index_header(root: &Path, id: u64) -> Result<Option<Header>>
```

**Consumption**: `next_X`, `drain_X`, `for_each_X`
```rust
fn next(&mut self) -> Result<Option<T>>
fn drain_available(&mut self, max: usize) -> Result<Vec<T>>
fn for_each_available<F>(&mut self, max: usize, f: F) -> Result<usize>
```

**Configuration**: `validate_X`, `store_X`, `set_X`
```rust
fn validate_segment_size(size: u64) -> Result<usize>
fn store_index(path: &Path, index: &SegmentIndex) -> Result<()>
fn set_segment_index(&self, segment: u32, offset: u64)
```

**Why This Helps**:
- LLM predicts method names: "To iterate, I'll use `for_each_available`"
- Grep becomes powerful: `rg "drain_\w+"`
- New developers learn patterns, not individual APIs

---

## 10. Example-Driven Documentation

### ❌ Before: Abstract Docs
```rust
/// Reads messages from the queue.
pub fn next(&mut self) -> Result<Option<MessageView<'_>>> {
    // ...
}
```

### ✅ After: Concrete Use Cases
```rust
/// Reads the next available message from the queue.
///
/// Returns `None` if no messages are available (non-blocking).
///
/// # Example
/// ```
/// let mut reader = Queue::open_subscriber(path, "my-reader")?;
/// while let Some(msg) = reader.next()? {
///     println!("seq={} payload={:?}", msg.seq, msg.payload);
/// }
/// ```
///
/// # Performance
/// - Latency: 50-150ns (data available)
/// - Allocation: Zero-copy, borrows from mmap
pub fn next(&mut self) -> Result<Option<MessageView<'_>>> {
    // ...
}
```

**Why This Helps**:
- LLM can copy-paste example and adapt it
- Performance characteristics guide optimization
- Common mistakes are avoided

---

## 11. Type-Driven Correctness

### ❌ Before: Runtime Validation
```rust
fn append(&mut self, type_id: u16, payload: &[u8]) -> Result<()> {
    if payload.len() > MAX_PAYLOAD_LEN {
        return Err(Error::PayloadTooLarge);
    }
    // ...
}
```

### ✅ After: Compile-Time Guarantees
```rust
pub struct PayloadBuffer<'a> {
    buf: &'a mut [u8],
}

impl<'a> PayloadBuffer<'a> {
    /// Creates a payload buffer of valid size.
    pub fn new(buf: &'a mut [u8]) -> Result<Self> {
        if buf.len() > MAX_PAYLOAD_LEN {
            return Err(Error::PayloadTooLarge);
        }
        Ok(Self { buf })
    }

    pub fn as_slice(&self) -> &[u8] {
        self.buf
    }
}

fn append_validated(&mut self, type_id: u16, payload: PayloadBuffer<'_>) -> Result<()> {
    // payload.len() is guaranteed valid by construction
    // ...
}
```

**Why This Helps**:
- LLM suggests creating `PayloadBuffer` first
- Type system encodes invariants
- Less runtime overhead

---

## 12. Searchable Magic Numbers

### ❌ Before: Inline Constants
```rust
let page_size = 4096;
let header_size = 64;
if offset % 64 != 0 {
    // ...
}
```

### ✅ After: Named Constants with Purpose
```rust
/// OS page size for mmap prefaulting.
pub const PAGE_SIZE: usize = 4096;

/// Message header size (cache-line aligned).
pub const HEADER_SIZE: usize = 64;

/// Record alignment (prevents false sharing).
pub const RECORD_ALIGN: usize = 64;

if offset % RECORD_ALIGN != 0 {
    // ...
}
```

**Why This Helps**:
- `grep RECORD_ALIGN` finds all alignment logic
- LLM understands *why* 64, not just *what* 64
- Platform-specific values centralized

---

## Anti-Patterns to Avoid

### 1. Over-Clever Code
```rust
// ❌ Unreadable
fn align_up(v: usize, a: usize) -> usize { (v + a - 1) & !(a - 1) }

// ✅ LLM-friendly (comment explains the trick if needed)
/// Aligns `value` up to the next multiple of `align`.
///
/// Uses bit manipulation for performance: (value + align - 1) & !(align - 1)
fn align_up(value: usize, align: usize) -> usize {
    if align == 0 { return value; }
    (value + align - 1) & !(align - 1)
}
```

### 2. Implicit Dependencies
```rust
// ❌ Global state
static CONFIG: OnceCell<Config> = OnceCell::new();

// ✅ Explicit injection
pub struct QueueWriter {
    config: WriterConfig,
    // ...
}
```

### 3. Meaningless Names
```rust
// ❌ What is tmp, val, res?
let tmp = load_data()?;
let val = process(tmp)?;
let res = validate(val)?;

// ✅ Self-documenting
let raw_segment = load_segment_data()?;
let parsed_header = parse_header(raw_segment)?;
let validated = validate_magic_and_version(parsed_header)?;
```

---

## Adoption Checklist

When reviewing code (human or LLM-generated), ask:

- [ ] Can I understand intent from type names alone?
- [ ] Are magic numbers named and documented?
- [ ] Do errors provide actionable context?
- [ ] Is configuration explicit, not global?
- [ ] Are functions < 100 lines with focused purpose?
- [ ] Do naming patterns generalize (drain_X, for_each_X)?
- [ ] Are trade-offs documented (latency vs. durability)?
- [ ] Would an LLM suggest correct usage from signatures?
- [ ] Is versioning explicit for wire formats?
- [ ] Are metrics exposed for observability?

---

## Case Study: chronicle-rs

This document was extracted from real refactoring work on chronicle-rs. Key improvements:

| Change | Before | After |
|--------|--------|-------|
| **Versioning** | Implicit format | `MSG_VERSION`, `SEG_VERSION` |
| **Configuration** | Hardcoded constants | `RetentionConfig` struct |
| **Observability** | Silent errors | `WriterMetrics` API |
| **Batch APIs** | Single message only | `next()`, `drain_available()`, `for_each_available()` |

**Result**: +212 lines, -34 lines, 100% test pass rate, better LLM understanding.

---

## References

- [Clean Code](https://www.amazon.com/Clean-Code-Handbook-Software-Craftsmanship/dp/0132350882) - Robert C. Martin
- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
- [Parse, don't validate](https://lexi-lambda.github.io/blog/2019/11/05/parse-don-t-validate/) - Alexis King

---

## Contributing

This is a living document. Submit PRs with:
- New patterns discovered
- Before/after examples from real code
- Counter-examples that didn't work

**Maintainer**: Chronicle-RS Team
**License**: CC-BY-4.0
