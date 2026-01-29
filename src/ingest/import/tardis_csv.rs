//! Tardis CSV import module.
//!
//! **BROKEN**: This module needs to be migrated to use `table::Table` instead of
//! the removed `storage::ArchiveWriter`. The code is commented out pending migration.

// compile_error!("tardis_csv import requires migration to table::Table (storage module removed)");

/*
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use blake3::Hasher;
use csv::{ReaderBuilder, StringRecord, Trim};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};

use crate::layout::ArchiveLayout;
use crate::protocol::{
    book_flags, BookEventHeader, BookEventType, BookMode, L2Diff, L2Snapshot, PriceLevelUpdate,
    TypeId, PROTOCOL_VERSION,
};
// REMOVED: use crate::storage::{write_meta_at_if_missing, ArchiveWriter, MetaFile, TierConfig, TierManager};

#[derive(Debug, Clone)]
pub struct ImportStats {
    pub rows: u64,
    pub groups: u64,
    pub segments: u64,
    pub min_ts_ns: u64,
    pub max_ts_ns: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct ImportLedger {
    source_path: String,
    file_hash: String,
    rows: u64,
    groups: u64,
    segments: u64,
    min_ts_ns: u64,
    max_ts_ns: u64,
    imported_at_ns: u64,
    venue: String,
    symbol: String,
    date: String,
    stream: String,
}

#[derive(Debug, Clone)]
struct Group {
    local_ts_us: u64,
    exchange_ts_us: u64,
    is_snapshot: bool,
    bids: Vec<(String, String)>,
    asks: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
struct ColumnIndices {
    exchange: usize,
    symbol: usize,
    timestamp: usize,
    local_timestamp: usize,
    is_snapshot: usize,
    side: usize,
    price: usize,
    amount: usize,
}

impl ColumnIndices {
    fn from_headers(headers: &StringRecord) -> Result<Self> {
        let lookup = |name: &str| -> Result<usize> {
            headers
                .iter()
                .position(|h| h == name)
                .ok_or_else(|| anyhow!("missing csv column: {name}"))
        };

        Ok(Self {
            exchange: lookup("exchange")?,
            symbol: lookup("symbol")?,
            timestamp: lookup("timestamp")?,
            local_timestamp: lookup("local_timestamp")?,
            is_snapshot: lookup("is_snapshot")?,
            side: lookup("side")?,
            price: lookup("price")?,
            amount: lookup("amount")?,
        })
    }
}

pub fn import_tardis_incremental_book(
    input: &Path,
    archive_root: &Path,
    venue: &str,
    symbol: &str,
    date: &str,
    stream: &str,
    venue_id: u16,
    market_id: u32,
    segment_size: usize,
    price_scale_override: Option<u8>,
    size_scale_override: Option<u8>,
    force: bool,
    compress: bool,
) -> Result<ImportStats> {
    validate_scale_override("price_scale", price_scale_override)?;
    validate_scale_override("size_scale", size_scale_override)?;
    let file_hash = hash_file(input)?;
    let ledger_path = ledger_path(archive_root, venue, symbol, date, &file_hash)?;
    if ledger_path.exists() && !force {
        let data = std::fs::read(&ledger_path)?;
        let ledger: ImportLedger = serde_json::from_slice(&data)?;
        return Ok(ImportStats {
            rows: ledger.rows,
            groups: ledger.groups,
            segments: ledger.segments,
            min_ts_ns: ledger.min_ts_ns,
            max_ts_ns: ledger.max_ts_ns,
        });
    }

    let reader = open_input(input)?;
    let mut csv = ReaderBuilder::new()
        .trim(Trim::All)
        .flexible(true)
        .from_reader(reader);
    let headers = csv.headers()?.clone();
    let indices = ColumnIndices::from_headers(&headers)?;

    let mut writer = ArchiveWriter::new(archive_root, venue, symbol, date, stream, segment_size)?;

    let mut stats = ImportStats {
        rows: 0,
        groups: 0,
        segments: 0,
        min_ts_ns: 0,
        max_ts_ns: 0,
    };

    let mut seq: u64 = 0;
    let mut current: Option<Group> = None;
    let mut seen_exchange: Option<String> = None;
    let mut seen_symbol: Option<String> = None;

    for record in csv.records() {
        let record = record?;
        let row = parse_row(&record, &indices)?;
        stats.rows = stats.rows.saturating_add(1);

        if seen_exchange.is_none() {
            seen_exchange = Some(row.exchange.clone());
        }
        if seen_symbol.is_none() {
            seen_symbol = Some(row.symbol.clone());
        }
        if let Some(ref exchange) = seen_exchange {
            if exchange != &row.exchange {
                return Err(anyhow!(
                    "mixed exchange values in input: {exchange} vs {}",
                    row.exchange
                ));
            }
        }
        if let Some(ref sym) = seen_symbol {
            if sym != &row.symbol {
                return Err(anyhow!(
                    "mixed symbol values in input: {sym} vs {}",
                    row.symbol
                ));
            }
        }
        if row.exchange != venue {
            return Err(anyhow!(
                "input exchange {} does not match venue {}",
                row.exchange,
                venue
            ));
        }
        if row.symbol != symbol {
            return Err(anyhow!(
                "input symbol {} does not match symbol {}",
                row.symbol,
                symbol
            ));
        }

        if let Some(group) = current.as_mut() {
            if group.local_ts_us == row.local_timestamp_us {
                merge_row(group, &row);
                continue;
            }

            let finished = current.take().expect("group exists");
            flush_group(
                &finished,
                &mut writer,
                venue_id,
                market_id,
                seq,
                price_scale_override,
                size_scale_override,
                &mut stats,
            )?;
            seq = seq.wrapping_add(1);
            current = Some(Group::from_row(row));
            continue;
        }

        current = Some(Group::from_row(row));
    }

    if let Some(group) = current {
        flush_group(
            &group,
            &mut writer,
            venue_id,
            market_id,
            seq,
            price_scale_override,
            size_scale_override,
            &mut stats,
        )?;
    }

    writer.finish()?;
    stats.segments = writer.segments_written();

    let event_time_range = if stats.min_ts_ns > 0 && stats.max_ts_ns > 0 {
        Some([stats.min_ts_ns, stats.max_ts_ns])
    } else {
        None
    };

    let meta = MetaFile {
        symbol_code: Some(symbol.to_string()),
        venue: Some(venue.to_string()),
        ingest_time_ns: now_ns(),
        event_time_range,
        completeness: "sealed".to_string(),
    };
    let layout = ArchiveLayout::new(archive_root);
    let meta_path = layout
        .stream_meta_path(venue, symbol, date, stream)
        .map_err(|e| anyhow!(e))?;
    write_meta_at_if_missing(&meta_path, &meta)?;

    if compress {
        let stream_dir = layout
            .stream_dir(venue, symbol, date, stream)
            .map_err(|e| anyhow!(e))?;
        let mut tier = TierManager::new(TierConfig::new(stream_dir));
        tier.run_once()?;
    }

    write_ledger(
        &ledger_path,
        input,
        &file_hash,
        venue,
        symbol,
        date,
        stream,
        &stats,
    )?;

    Ok(stats)
}

impl Group {
    fn from_row(row: TardisRow) -> Self {
        let exchange_ts_us = if row.timestamp_us == 0 {
            row.local_timestamp_us
        } else {
            row.timestamp_us
        };
        let mut group = Self {
            local_ts_us: row.local_timestamp_us,
            exchange_ts_us,
            is_snapshot: row.is_snapshot,
            bids: Vec::new(),
            asks: Vec::new(),
        };
        group.push_side(row.side, row.price, row.amount);
        group
    }

    fn push_side(&mut self, side: Side, price: String, amount: String) {
        match side {
            Side::Bid => self.bids.push((price, amount)),
            Side::Ask => self.asks.push((price, amount)),
        }
    }
}

fn merge_row(group: &mut Group, row: &TardisRow) {
    let exchange_ts_us = if row.timestamp_us == 0 {
        row.local_timestamp_us
    } else {
        row.timestamp_us
    };
    group.exchange_ts_us = group.exchange_ts_us.max(exchange_ts_us);
    group.is_snapshot = group.is_snapshot || row.is_snapshot;
    group.push_side(row.side, row.price.clone(), row.amount.clone());
}

#[derive(Debug, Clone)]
struct TardisRow {
    exchange: String,
    symbol: String,
    timestamp_us: u64,
    local_timestamp_us: u64,
    is_snapshot: bool,
    side: Side,
    price: String,
    amount: String,
}

fn parse_row(record: &StringRecord, indices: &ColumnIndices) -> Result<TardisRow> {
    let exchange = field(record, indices.exchange, "exchange")?.to_string();
    let symbol = field(record, indices.symbol, "symbol")?.to_string();
    let timestamp_us = parse_u64(field(record, indices.timestamp, "timestamp")?)?;
    let local_timestamp_us = parse_u64(field(record, indices.local_timestamp, "local_timestamp")?)?;
    let is_snapshot = parse_bool(field(record, indices.is_snapshot, "is_snapshot")?)?;
    let side = parse_side(field(record, indices.side, "side")?)?;
    let price = field(record, indices.price, "price")?.to_string();
    let amount = field(record, indices.amount, "amount")?.to_string();

    if local_timestamp_us == 0 {
        return Err(anyhow!("local_timestamp must be non-zero"));
    }

    Ok(TardisRow {
        exchange,
        symbol,
        timestamp_us,
        local_timestamp_us,
        is_snapshot,
        side,
        price,
        amount,
    })
}

fn flush_group(
    group: &Group,
    writer: &mut ArchiveWriter,
    venue_id: u16,
    market_id: u32,
    seq: u64,
    price_scale_override: Option<u8>,
    size_scale_override: Option<u8>,
    stats: &mut ImportStats,
) -> Result<()> {
    let ingest_ts_ns = group.local_ts_us.saturating_mul(1_000);
    let exchange_ts_ns = group.exchange_ts_us.saturating_mul(1_000);

    let (mut price_scale, mut size_scale) = max_scales(&group.bids, &group.asks);
    if let Some(scale) = price_scale_override {
        price_scale = scale;
    }
    if let Some(scale) = size_scale_override {
        size_scale = scale;
    }
    let bids = parse_levels(&group.bids, price_scale, size_scale);
    let asks = parse_levels(&group.asks, price_scale, size_scale);

    let payload = if group.is_snapshot {
        encode_snapshot(
            venue_id,
            market_id,
            seq,
            ingest_ts_ns,
            exchange_ts_ns,
            price_scale,
            size_scale,
            &bids,
            &asks,
        )?
    } else {
        encode_diff(
            venue_id,
            market_id,
            seq,
            ingest_ts_ns,
            exchange_ts_ns,
            price_scale,
            size_scale,
            &bids,
            &asks,
        )?
    };

    writer.append_record(TypeId::BookEvent.as_u16(), ingest_ts_ns, &payload)?;

    stats.groups = stats.groups.saturating_add(1);
    stats.min_ts_ns = if stats.min_ts_ns == 0 {
        exchange_ts_ns
    } else {
        stats.min_ts_ns.min(exchange_ts_ns)
    };
    stats.max_ts_ns = stats.max_ts_ns.max(exchange_ts_ns);

    Ok(())
}

fn encode_snapshot(
    venue_id: u16,
    market_id: u32,
    seq: u64,
    ingest_ts_ns: u64,
    exchange_ts_ns: u64,
    price_scale: u8,
    size_scale: u8,
    bids: &[PriceLevelUpdate],
    asks: &[PriceLevelUpdate],
) -> Result<Vec<u8>> {
    let bid_count =
        u32::try_from(bids.len()).map_err(|_| anyhow!("snapshot bid_count exceeds u32"))?;
    let ask_count =
        u32::try_from(asks.len()).map_err(|_| anyhow!("snapshot ask_count exceeds u32"))?;

        println!(
            "encode_snapshot: price_scale = {}, size_scale = {}",
            price_scale, size_scale
        );
        println!(
            "encode_snapshot: bid_count = {}, ask_count = {}",
            bid_count, ask_count
        );
    let record_len = book_event_len::<L2Snapshot>(bids.len(), asks.len())?;

    let header = BookEventHeader {
        schema_version: PROTOCOL_VERSION,
        record_len,
        endianness: 0,
        _pad0: 0,
        venue_id,
        market_id,
        stream_id: 0,
        ingest_ts_ns,
        exchange_ts_ns,
        seq,
        native_seq: seq,
        event_type: BookEventType::Snapshot as u8,
        book_mode: BookMode::L2 as u8,
        flags: 0,
        _pad1: 0,
    };
    let snapshot = L2Snapshot {
        price_scale,
        size_scale,
        _pad0: 0,
        bid_count,
        ask_count,
    };



    let total_len = std::mem::size_of::<BookEventHeader>()
        + std::mem::size_of::<L2Snapshot>()
        + (bids.len() + asks.len()) * std::mem::size_of::<PriceLevelUpdate>();
    let mut buf = vec![0u8; total_len];
    let mut offset = 0;
    write_struct(&mut buf, offset, &header);
    offset += std::mem::size_of::<BookEventHeader>();
    write_struct(&mut buf, offset, &snapshot);
    offset += std::mem::size_of::<L2Snapshot>();
    offset = write_levels(&mut buf, offset, bids);
    let _ = write_levels(&mut buf, offset, asks);
    Ok(buf)
}

fn encode_diff(
    venue_id: u16,
    market_id: u32,
    seq: u64,
    ingest_ts_ns: u64,
    exchange_ts_ns: u64,
    price_scale: u8,
    size_scale: u8,
    bids: &[PriceLevelUpdate],
    asks: &[PriceLevelUpdate],
) -> Result<Vec<u8>> {
    let bid_count = u16::try_from(bids.len()).map_err(|_| anyhow!("diff bid_count exceeds u16"))?;
    let ask_count = u16::try_from(asks.len()).map_err(|_| anyhow!("diff ask_count exceeds u16"))?;
    let record_len = book_event_len::<L2Diff>(bids.len(), asks.len())?;

    let header = BookEventHeader {
        schema_version: PROTOCOL_VERSION,
        record_len,
        endianness: 0,
        _pad0: 0,
        venue_id,
        market_id,
        stream_id: 0,
        ingest_ts_ns,
        exchange_ts_ns,
        seq,
        native_seq: seq,
        event_type: BookEventType::Diff as u8,
        book_mode: BookMode::L2 as u8,
        flags: 0,
        _pad1: 0,
    };
    let diff = L2Diff {
        update_id_first: seq,
        update_id_last: seq,
        update_id_prev: seq.saturating_sub(1),
        price_scale,
        size_scale,
        flags: book_flags::ABSOLUTE,
        bid_count,
        ask_count,
    };

    let total_len = std::mem::size_of::<BookEventHeader>()
        + std::mem::size_of::<L2Diff>()
        + (bids.len() + asks.len()) * std::mem::size_of::<PriceLevelUpdate>();
    let mut buf = vec![0u8; total_len];
    let mut offset = 0;
    write_struct(&mut buf, offset, &header);
    offset += std::mem::size_of::<BookEventHeader>();
    write_struct(&mut buf, offset, &diff);
    offset += std::mem::size_of::<L2Diff>();
    offset = write_levels(&mut buf, offset, bids);
    let _ = write_levels(&mut buf, offset, asks);
    Ok(buf)
}

fn parse_levels(
    levels: &[(String, String)],
    price_scale: u8,
    size_scale: u8,
) -> Vec<PriceLevelUpdate> {
    levels
        .iter()
        .map(|(price, size)| PriceLevelUpdate {
            price: parse_fixed(price, price_scale),
            size: parse_fixed(size, size_scale),
        })
        .collect()
}

fn book_event_len<T>(bid_len: usize, ask_len: usize) -> Result<u32> {
    let total = std::mem::size_of::<BookEventHeader>()
        + std::mem::size_of::<T>()
        + (bid_len + ask_len) * std::mem::size_of::<PriceLevelUpdate>();
    u32::try_from(total).map_err(|_| anyhow!("BookEvent record_len exceeds u32"))
}

fn write_struct<T>(buf: &mut [u8], offset: usize, value: &T) {
    let size = std::mem::size_of::<T>();
    let ptr = value as *const T as *const u8;
    unsafe {
        let src = std::slice::from_raw_parts(ptr, size);
        buf[offset..offset + size].copy_from_slice(src);
    }
}

fn write_levels(buf: &mut [u8], mut offset: usize, levels: &[PriceLevelUpdate]) -> usize {
    let size = levels.len() * std::mem::size_of::<PriceLevelUpdate>();
    if size == 0 {
        return offset;
    }
    let ptr = levels.as_ptr() as *const u8;
    unsafe {
        let src = std::slice::from_raw_parts(ptr, size);
        buf[offset..offset + size].copy_from_slice(src);
    }
    offset += size;
    offset
}

fn max_scales(bids: &[(String, String)], asks: &[(String, String)]) -> (u8, u8) {
    let mut price_scale = 0u8;
    let mut size_scale = 0u8;
    for (price, size) in bids.iter().chain(asks.iter()) {
        price_scale = price_scale.max(decimal_scale(price));
        size_scale = size_scale.max(decimal_scale(size));
    }
    (price_scale, size_scale)
}

fn decimal_scale(value: &str) -> u8 {
    let Some((_, frac)) = value.split_once('.') else {
        return 0;
    };
    let trimmed = frac.trim_end_matches('0');
    let len = trimmed.len();
    u8::try_from(len.min(18)).unwrap_or(18)
}

fn parse_fixed(value: &str, scale: u8) -> u64 {
    let scale = scale.min(18);
    let pow10 = POW10[scale as usize];
    let mut iter = value.splitn(2, '.');
    let int_part = iter.next().unwrap_or("0");
    let frac_part = iter.next().unwrap_or("");
    let frac_trimmed = frac_part.trim_end_matches('0');

    let int_val = int_part.parse::<u128>().unwrap_or(0);
    let mut frac_val = 0u128;
    let mut frac_len = 0usize;
    if !frac_trimmed.is_empty() {
        frac_len = frac_trimmed.len().min(scale as usize);
        frac_val = frac_trimmed[..frac_len].parse::<u128>().unwrap_or(0);
    }

    let scaled = int_val.saturating_mul(pow10 as u128).saturating_add(
        frac_val.saturating_mul(POW10[(scale as usize).saturating_sub(frac_len)] as u128),
    );
    scaled.min(u64::MAX as u128) as u64
}

#[derive(Debug, Clone, Copy)]
enum Side {
    Bid,
    Ask,
}

fn parse_side(side: &str) -> Result<Side> {
    match side.to_ascii_lowercase().as_str() {
        "bid" | "buy" => Ok(Side::Bid),
        "ask" | "sell" => Ok(Side::Ask),
        other => Err(anyhow!("invalid side: {other}")),
    }
}

fn field<'a>(record: &'a StringRecord, idx: usize, name: &str) -> Result<&'a str> {
    record
        .get(idx)
        .ok_or_else(|| anyhow!("missing field {name}"))
}

fn parse_u64(value: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .map_err(|_| anyhow!("invalid u64: {value}"))
}

fn parse_bool(value: &str) -> Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        other => Err(anyhow!("invalid bool: {other}")),
    }
}

fn validate_scale_override(name: &str, value: Option<u8>) -> Result<()> {
    if let Some(scale) = value {
        if scale > 18 {
            return Err(anyhow!("{name} must be <= 18"));
        }
    }
    Ok(())
}

fn open_input(path: &Path) -> Result<Box<dyn Read>> {
    let file = File::open(path).with_context(|| format!("open input {}", path.display()))?;
    if path.extension().and_then(|ext| ext.to_str()) == Some("gz") {
        Ok(Box::new(GzDecoder::new(file)))
    } else {
        Ok(Box::new(file))
    }
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("hash input {}", path.display()))?;
    let mut hasher = Hasher::new();
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn ledger_path(
    archive_root: &Path,
    venue: &str,
    symbol: &str,
    date: &str,
    file_hash: &str,
) -> Result<PathBuf> {
    let dir = archive_root
        .join("imports")
        .join("v1")
        .join("tardis")
        .join(venue)
        .join(symbol)
        .join(date);
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{file_hash}.json")))
}

fn write_ledger(
    path: &Path,
    input: &Path,
    file_hash: &str,
    venue: &str,
    symbol: &str,
    date: &str,
    stream: &str,
    stats: &ImportStats,
) -> Result<()> {
    let ledger = ImportLedger {
        source_path: input.display().to_string(),
        file_hash: file_hash.to_string(),
        rows: stats.rows,
        groups: stats.groups,
        segments: stats.segments,
        min_ts_ns: stats.min_ts_ns,
        max_ts_ns: stats.max_ts_ns,
        imported_at_ns: now_ns(),
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        date: date.to_string(),
        stream: stream.to_string(),
    };

    let data = serde_json::to_vec_pretty(&ledger)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

const POW10: [u64; 19] = [
    1,
    10,
    100,
    1_000,
    10_000,
    100_000,
    1_000_000,
    10_000_000,
    100_000_000,
    1_000_000_000,
    10_000_000_000,
    100_000_000_000,
    1_000_000_000_000,
    10_000_000_000_000,
    100_000_000_000_000,
    1_000_000_000_000_000,
    10_000_000_000_000_000,
    100_000_000_000_000_000,
    1_000_000_000_000_000_000,
];
*/
