use super::catalog::{SymbolCatalog, SymbolIdentity};
use crate::core::{Queue, QueueWriter, WriterConfig};
use crate::protocol::{
    book_flags, BookEventHeader, BookEventType, BookMode, L2Snapshot, PriceLevelUpdate, TypeId,
    PROTOCOL_VERSION,
};
use crate::replay::{L2Book, ReplayEngine, ReplayMessage};
use anyhow::{anyhow, Result};
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;

pub struct Refinery {
    engine: ReplayEngine,
    writer: QueueWriter,
    snapshot_interval_ns: u64,
    last_snapshot_ns: u64,
    catalog: Arc<RwLock<SymbolCatalog>>,
    active_symbol: Option<SymbolIdentity>,
    active_market_id: Option<u32>,
    active_venue_id: Option<u16>,
}

impl Refinery {
    pub fn new(
        input_path: impl AsRef<Path>,
        reader_name: &str,
        output_path: impl AsRef<Path>,
        snapshot_interval: Duration,
        catalog: Arc<RwLock<SymbolCatalog>>,
    ) -> Result<Self> {
        let engine = ReplayEngine::open(input_path, reader_name)?;

        // Use Archive config for output (flushes index frequently)
        let config = WriterConfig::archive();
        let writer = Queue::open_publisher_with_config(output_path, config)?;

        Ok(Self {
            engine,
            writer,
            snapshot_interval_ns: snapshot_interval.as_nanos() as u64,
            last_snapshot_ns: 0,
            catalog,
            active_symbol: None,
            active_market_id: None,
            active_venue_id: None,
        })
    }

    pub fn run(&mut self) -> Result<()> {
        let engine = &mut self.engine;
        let writer = &mut self.writer;
        let snapshot_interval_ns = self.snapshot_interval_ns;
        let last_snapshot_ns = &mut self.last_snapshot_ns;
        let catalog = Arc::clone(&self.catalog);
        let active_symbol = &mut self.active_symbol;
        let active_market_id = &mut self.active_market_id;
        let active_venue_id = &mut self.active_venue_id;

        while let Some(msg) = engine.next_message()? {
            let Some((venue_id, market_id, event_ts_ns)) = extract_book_identity(&msg) else {
                drop(msg);
                engine.commit()?;
                continue;
            };

            let symbol = {
                let guard = catalog
                    .read()
                    .map_err(|_| anyhow!("symbol catalog lock poisoned"))?;
                guard
                    .resolve_by_market_id(venue_id, market_id, event_ts_ns)
                    .ok_or_else(|| {
                        anyhow!(
                            "symbol not found for venue_id={} market_id={} event_ts_ns={}",
                            venue_id,
                            market_id,
                            event_ts_ns
                        )
                    })?
            };

            ensure_active_symbol(
                active_symbol,
                active_market_id,
                active_venue_id,
                venue_id,
                market_id,
                &symbol,
            )?;

            let should_snapshot = if *last_snapshot_ns == 0 {
                true
            } else {
                event_ts_ns.saturating_sub(*last_snapshot_ns) >= snapshot_interval_ns
            };

            if should_snapshot {
                inject_snapshot(writer, msg.book, event_ts_ns, venue_id, market_id)?;
                *last_snapshot_ns = event_ts_ns;
            }

            writer.append_with_timestamp(msg.msg.type_id, msg.msg.payload, msg.msg.timestamp_ns)?;

            drop(msg);
            engine.commit()?;
        }
        Ok(())
    }
}

fn inject_snapshot(
    writer: &mut QueueWriter,
    book: &L2Book,
    timestamp_ns: u64,
    venue_id: u16,
    market_id: u32,
) -> Result<()> {
    // 1. Calculate size
    let bid_count = book.bids().len();
    let ask_count = book.asks().len();
    let (price_scale, size_scale) = book.scales();

    let header_size = std::mem::size_of::<BookEventHeader>();
    let snapshot_size = std::mem::size_of::<L2Snapshot>();
    let level_size = std::mem::size_of::<PriceLevelUpdate>();
    let payload_len = header_size + snapshot_size + (bid_count + ask_count) * level_size;
    let record_len =
        u32::try_from(payload_len).map_err(|_| anyhow!("BookEvent record_len exceeds u32"))?;

    writer.append_in_place_with_timestamp(
        TypeId::BookEvent.as_u16(),
        payload_len,
        timestamp_ns,
        |buf| {
            // Write Header
            let header = BookEventHeader {
                schema_version: PROTOCOL_VERSION,
                record_len,
                endianness: 0,
                _pad0: 0,
                venue_id,
                market_id,
                stream_id: 0,
                ingest_ts_ns: timestamp_ns,
                exchange_ts_ns: timestamp_ns,
                seq: 0,
                native_seq: 0,
                event_type: BookEventType::Snapshot as u8,
                book_mode: BookMode::L2 as u8,
                flags: book_flags::ABSOLUTE,
                _pad1: 0,
            };
            // Safety: We simply cast the struct to bytes. Protocol crate should handle this.
            // For now, manual serialization to avoid dependency hell in this snippet.
            let header_bytes = unsafe {
                std::slice::from_raw_parts(&header as *const _ as *const u8, header_size)
            };
            buf[0..header_size].copy_from_slice(header_bytes);

            // Write Snapshot Body
            let snapshot = L2Snapshot {
                bid_count: bid_count as u32,
                ask_count: ask_count as u32,
                price_scale,
                size_scale,
                _pad0: 0,
            };
            let snapshot_bytes = unsafe {
                std::slice::from_raw_parts(&snapshot as *const _ as *const u8, snapshot_size)
            };
            let offset = header_size;
            buf[offset..offset + snapshot_size].copy_from_slice(snapshot_bytes);

            // Write Levels
            let mut offset = header_size + snapshot_size;
            for (price, size) in book.bids().iter().rev() {
                // Bids desc
                let level = PriceLevelUpdate {
                    price: *price,
                    size: *size,
                };
                let bytes = unsafe { std::mem::transmute::<_, [u8; 16]>(level) };
                buf[offset..offset + 16].copy_from_slice(&bytes);
                offset += 16;
            }
            for (price, size) in book.asks().iter() {
                // Asks asc
                let level = PriceLevelUpdate {
                    price: *price,
                    size: *size,
                };
                let bytes = unsafe { std::mem::transmute::<_, [u8; 16]>(level) };
                buf[offset..offset + 16].copy_from_slice(&bytes);
                offset += 16;
            }

            Ok(())
        },
    )?;
    Ok(())
}

fn ensure_active_symbol(
    active_symbol: &mut Option<SymbolIdentity>,
    active_market_id: &mut Option<u32>,
    active_venue_id: &mut Option<u16>,
    venue_id: u16,
    market_id: u32,
    symbol: &SymbolIdentity,
) -> Result<()> {
    match (active_symbol.as_ref(), *active_market_id, *active_venue_id) {
        (None, None, None) => {
            *active_symbol = Some(symbol.clone());
            *active_market_id = Some(market_id);
            *active_venue_id = Some(venue_id);
            Ok(())
        }
        (Some(existing), Some(existing_market), Some(existing_venue)) => {
            if existing_market != market_id || existing_venue != venue_id {
                return Err(anyhow!(
                    "refinery only supports a single symbol per run (got venue_id={} market_id={}, expected venue_id={} market_id={})",
                    venue_id,
                    market_id,
                    existing_venue,
                    existing_market
                ));
            }
            if existing.symbol_id != symbol.symbol_id {
                return Err(anyhow!(
                    "symbol_id mismatch for venue_id={} market_id={}: {} vs {}",
                    venue_id,
                    market_id,
                    existing.symbol_id,
                    symbol.symbol_id
                ));
            }
            Ok(())
        }
        _ => Err(anyhow!("refinery active symbol state inconsistent")),
    }
}

fn extract_book_identity(msg: &ReplayMessage<'_>) -> Option<(u16, u32, u64)> {
    let header = msg.book_event?.header;
    Some((header.venue_id, header.market_id, header.exchange_ts_ns))
}
