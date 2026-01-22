# Feature Extraction Pipeline Architecture (The Silas Standard)

## 1. Objective: High-Throughput ETL

This document defines the architecture for the **Chronicle Feature Extractor**, a high-performance pipeline designed to transform raw L3/L2 market logs into structured datasets (Parquet/Arrow) for machine learning training.

**Philosophy:**
- **Batch over Event:** Unlike backtesting (event-driven), extraction is throughput-driven.
- **Zero-Copy to Column:** Compute features in registers and write directly to Arrow buffers.
- **GIL-Free:** Python configures the job; Rust executes the loop.

## 2. Pipeline Architecture

The pipeline consists of three stages executed in a tight loop:
1.  **Ingest:** Zero-copy read from Chronicle Queue (`.q` files).
2.  **Reconstruct:** deterministic L3/L2 book updates.
3.  **Compute & Emit:** Calculate features and append to columnar buffers.

```mermaid
graph LR
    Log[Chronicle Log] -->|mmap| Reader[QueueReader]
    Reader -->|Event| Book[L3 OrderBook]
    Book -->|State| Extractor[FeatureSet]
    Reader -->|Event| Extractor
    Extractor -->|Row| Arrow[Arrow Builder]
    Arrow -->|Batch| Parquet[Parquet Writer]
```

## 3. Rust API: The `FeatureSet` Trait

The core of the system is the `FeatureSet` trait. It defines the schema and the transformation logic.

### 3.1 Trait Definition

```rust
pub trait FeatureSet {
    /// Define the output schema (Column Names and Types)
    fn schema(&self) -> Schema;

    /// Called on every market event.
    /// - `book`: The current state of the Order Book (post-update).
    /// - `event`: The event that triggered the update (Add/Cancel/Trade).
    /// - `out`: The row buffer to write features into.
    fn calculate(&mut self, book: &L3OrderBook, event: &BookEvent, out: &mut RowBuffer);
    
    /// Optional: Filter events to emit. (e.g., only emit on Trades or Time Bars)
    fn should_emit(&self, event: &BookEvent) -> bool {
        true // Default: emit every tick
    }
}
```

### 3.2 Example: Order Flow Imbalance (OFI)

```rust
pub struct OfiExtractor {
    prev_bid: f64,
    prev_ask: f64,
    prev_bid_sz: f64,
    prev_ask_sz: f64,
}

impl FeatureSet for OfiExtractor {
    fn calculate(&mut self, book: &L3OrderBook, event: &BookEvent, out: &mut RowBuffer) {
        let bid = book.best_bid_price();
        let ask = book.best_ask_price();
        let bid_sz = book.best_bid_size();
        let ask_sz = book.best_ask_size();

        // Calculate OFI components (e_n)
        let e_bid = if bid > self.prev_bid {
            bid_sz
        } else if bid < self.prev_bid {
            -self.prev_bid_sz
        } else {
            bid_sz - self.prev_bid_sz
        };
        
        let e_ask = if ask < self.prev_ask {
            ask_sz
        } else if ask > self.prev_ask {
            -self.prev_ask_sz
        } else {
            ask_sz - self.prev_ask_sz
        };

        let ofi = e_bid - e_ask;

        // Direct write to Arrow buffer (no intermediate allocation)
        out.append_f64(0, ofi);
        out.append_f64(1, (ask - bid) / ((ask + bid) / 2.0)); // Spread bps

        // Update state
        self.prev_bid = bid;
        self.prev_ask = ask;
        self.prev_bid_sz = bid_sz;
        self.prev_ask_sz = ask_sz;
    }
}
```

## 4. Python API: The Analyst Interface

Researchers define the pipeline in Python, but execution happens in Rust.

### 4.1 Configuration
We expose a library of optimized feature primitives.

```python
import chronicle_rs as ch

# Define the Feature Set
# These map to pre-compiled Rust structs for maximum speed.
config = ch.ExtractionConfig(
    features=[
        ch.features.GlobalTime(),            # ingest_ts, exchange_ts
        ch.features.MidPrice(),
        ch.features.BookImbalance(depth=5),  # (BidVol - AskVol) / Total
        ch.features.OFI(decay=0.0),          # Raw OFI
        ch.features.TradeFlow(),             # Aggressor volume
    ],
    # Filters control the output resolution
    trigger=ch.triggers.TradeOrTime(sec=1.0) 
)
```

### 4.2 Execution (Batch Processing)

```python
# Run extraction
# This releases the GIL. Progress is reported via callbacks or TQDM.
ch.extract_to_parquet(
    source="/data/chronicle/binance_spot",
    symbol="BTCUSDT",
    destination="data/features/btc_20240120.parquet",
    config=config,
    compression="snappy"
)
```

## 5. Implementation Strategy

### 5.1 The "RowBuffer" Abstraction
To avoid allocating a `Vec<f64>` for every row, `RowBuffer` is a facade over a set of `arrow::array::PrimitiveBuilder`.
- `out.append_f64(col_idx, value)` maps directly to `builders[col_idx].append_value(value)`.
- When builders reach a batch size (e.g., 8192 rows), they are flushed to the Parquet writer.

### 5.2 Parallelism
Since L3 reconstruction is strictly serial (causal), we cannot parallelize within a single symbol stream.
- **Parallelism Strategy:** **Multi-Symbol Processing.**
- Use `rayon` to process multiple symbols (files) concurrently, saturating the NVMe bandwidth.

### 5.3 Warm Starts (Snapshots)
The Extractor must support the same `Snapshot` mechanism as the Replay Engine.
- If the job starts at `10:00`, it loads the `09:XX` snapshot, fast-forwards to `10:00`, and *then* begins emitting rows.

## 6. Performance Targets
- **Throughput:** > 1,000,000 events/sec per core.
- **Memory:** Zero allocation during the loop (except Arrow page buffers).
- **Latency:** N/A (Throughput is king).

## 7. Future: Online Features
This same `FeatureSet` trait can be reused in the live strategy engine.
- **Offline:** `FeatureSet` writes to Parquet.
- **Online:** `FeatureSet` writes to a circular buffer for the Strategy model input.
- **Guarantee:** Training data and Inference data are bit-identical (Code Reuse).
