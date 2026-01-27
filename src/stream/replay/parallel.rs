use crate::stream::{StreamReader, StreamMessageOwned};
use crate::protocol::{TypeId, BookEventHeader};
use anyhow::Result;
use std::collections::{HashMap, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Per-symbol event handler (user-provided logic).
pub trait SymbolHandler: Send {
    /// Process a message for this symbol.
    fn handle(&mut self, msg: &StreamMessageOwned) -> Result<()>;

    /// Flush any buffered state.
    fn flush(&mut self) -> Result<()>;
}

/// Worker statistics.
#[derive(Debug, Clone, Default)]
pub struct WorkerStats {
    pub messages_processed: u64,
    pub symbols_processed: usize,
}

/// Overall replay statistics.
#[derive(Debug)]
pub struct MultiReplayStats {
    pub total_messages: u64,
    pub worker_stats: Vec<WorkerStats>,
    pub duration: Duration,
}

impl MultiReplayStats {
    pub fn throughput(&self) -> f64 {
        if self.duration.as_secs_f64() > 0.0 {
            self.total_messages as f64 / self.duration.as_secs_f64()
        } else {
            0.0
        }
    }
}

/// Parallel replay with consistent symbol-to-worker hashing.
pub struct MultiSymbolReplay<R, H>
where
    R: StreamReader,
    H: Fn(&str) -> Box<dyn SymbolHandler> + Send + Sync + 'static,
{
    source: R,
    worker_count: usize,
    handler_factory: Arc<H>,
    channel_capacity: usize,
}

impl<R, H> MultiSymbolReplay<R, H>
where
    R: StreamReader,
    H: Fn(&str) -> Box<dyn SymbolHandler> + Send + Sync + 'static,
{
    /// Create a multi-symbol replay with a handler factory.
    pub fn new(source: R, worker_count: usize, handler_factory: H) -> Self {
        Self {
            source,
            worker_count,
            handler_factory: Arc::new(handler_factory),
            channel_capacity: 1000,
        }
    }

    /// Set channel capacity (default: 1000).
    pub fn channel_capacity(mut self, capacity: usize) -> Self {
        self.channel_capacity = capacity;
        self
    }

    /// Run the replay with consistent hashing.
    pub fn run(mut self) -> Result<MultiReplayStats> {
        let worker_count = self.worker_count;
        let channel_capacity = self.channel_capacity;

        // Create channels for each worker
        let mut worker_txs = Vec::new();
        let mut worker_handles = Vec::new();

        for worker_id in 0..worker_count {
            let (tx, rx) = mpsc::sync_channel::<StreamMessageOwned>(channel_capacity);
            worker_txs.push(tx);

            let factory = Arc::clone(&self.handler_factory);
            let handle = thread::Builder::new()
                .name(format!("replay-worker-{}", worker_id))
                .spawn(move || -> Result<WorkerStats> {
                    // Per-symbol handlers (lazy-initialized)
                    let mut handlers: HashMap<String, Box<dyn SymbolHandler>> = HashMap::new();
                    let mut stats = WorkerStats::default();

                    while let Ok(msg) = rx.recv() {
                        // Extract symbol from payload
                        let symbol = extract_symbol(&msg)?;

                        // Get or create handler for this symbol
                        let handler = handlers
                            .entry(symbol.clone())
                            .or_insert_with(|| factory(&symbol));

                        handler.handle(&msg)?;
                        stats.messages_processed += 1;
                    }

                    // Flush all handlers
                    for (_symbol, handler) in handlers.iter_mut() {
                        handler.flush()?;
                    }
                    stats.symbols_processed = handlers.len();

                    Ok(stats)
                })
                .map_err(|e| anyhow::anyhow!("Failed to spawn worker thread: {}", e))?;

            worker_handles.push(handle);
        }

        // Dispatch loop: read messages and route to workers
        let mut total_messages = 0;
        let start = Instant::now();

        while let Some(msg) = self.source.next()? {
            // Convert to owned
            let owned = StreamMessageOwned {
                seq: msg.seq,
                timestamp_ns: msg.timestamp_ns,
                type_id: msg.type_id,
                payload: msg.payload.to_vec(),
            };

            // Extract symbol and hash to worker
            let symbol = extract_symbol(&owned)?;
            let worker_id = hash_symbol_to_worker(&symbol, worker_count);

            // Send to designated worker (blocks if channel full)
            worker_txs[worker_id]
                .send(owned)
                .map_err(|e| anyhow::anyhow!("Failed to send message to worker: {}", e))?;

            total_messages += 1;

            if total_messages % 100_000 == 0 {
                let elapsed = start.elapsed().as_secs_f64();
                let throughput = total_messages as f64 / elapsed;
                log::info!("Dispatched {} msgs ({:.0} msg/sec)", total_messages, throughput);
            }
        }

        // Close all channels
        drop(worker_txs);

        // Wait for workers and collect stats
        let mut worker_stats = Vec::new();
        for handle in worker_handles {
            worker_stats.push(
                handle
                    .join()
                    .map_err(|_| anyhow::anyhow!("worker thread panicked"))??,
            );
        }

        Ok(MultiReplayStats {
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

/// Extract symbol from message based on type_id.
fn extract_symbol(msg: &StreamMessageOwned) -> Result<String> {
    match msg.type_id {
        t if t == TypeId::BookEvent as u16 => {
            // Parse BookEventHeader to get market_id
            if msg.payload.len() < std::mem::size_of::<BookEventHeader>() {
                return Err(anyhow::anyhow!("payload too short for BookEventHeader"));
            }

            let header = unsafe {
                &*(msg.payload.as_ptr() as *const BookEventHeader)
            };

            // Use market_id as symbol (or map to symbol via lookup)
            Ok(format!("MARKET_{}", header.market_id))
        }
        _ => {
            // For non-BookEvent messages, use type_id as a fallback
            Ok(format!("TYPE_{}", msg.type_id))
        }
    }
}
