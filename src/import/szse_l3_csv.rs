use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use blake3::Hasher;
use csv::{ReaderBuilder, StringRecord, Trim};
use serde::{Deserialize, Serialize};
use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};

use crate::layout::ARCHIVE_VERSION;
use crate::protocol::{
    l3_flags, BookEventHeader, BookEventType, BookMode, L3Event, L3EventType, L3Side, TypeId,
    PROTOCOL_VERSION,
};
use crate::storage::{write_meta_at_if_missing, ArchiveWriter, MetaFile, TierConfig, TierManager};

const VENUE: &str = "szse";
const PRICE_SCALE: u8 = 3;
const SIZE_SCALE: u8 = 0;

#[derive(Debug, Clone)]
pub struct ImportStats {
    pub rows: u64,
    pub segments: u64,
    pub min_ts_ns: u64,
    pub max_ts_ns: u64,
    pub channel: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct ImportLedger {
    source_path: String,
    file_hash: String,
    rows: u64,
    segments: u64,
    min_ts_ns: u64,
    max_ts_ns: u64,
    imported_at_ns: u64,
    venue: String,
    date: String,
    channel: u32,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct ChannelIndex {
    date: String,
    updated_at_ns: u64,
    channel_to_symbols: BTreeMap<String, Vec<String>>,
    symbol_to_channel: BTreeMap<String, u32>,
}

#[derive(Debug, Clone)]
struct ColumnIndices {
    channel: usize,
    seq: usize,
    event: usize,
    symbol: usize,
    order_id: usize,
    bid_order_id: usize,
    ask_order_id: usize,
    side: usize,
    price: usize,
    qty: usize,
    amt: usize,
    ord_type: usize,
    exec_type: usize,
    transact_time: usize,
    sending_time: usize,
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
            channel: lookup("ChannelNo")?,
            seq: lookup("ApplSeqNum")?,
            event: lookup("Event")?,
            symbol: lookup("Symbol")?,
            order_id: lookup("OrderID")?,
            bid_order_id: lookup("BidOrderID")?,
            ask_order_id: lookup("OfferOrderID")?,
            side: lookup("Side")?,
            price: lookup("Price")?,
            qty: lookup("Qty")?,
            amt: lookup("Amt")?,
            ord_type: lookup("OrdType")?,
            exec_type: lookup("ExecType")?,
            transact_time: lookup("TransactTime")?,
            sending_time: lookup("SendingTime")?,
        })
    }
}

pub fn import_szse_l3_channel(
    input: &Path,
    archive_root: &Path,
    date: &str,
    channel: Option<u32>,
    segment_size: usize,
    force: bool,
    compress: bool,
) -> Result<ImportStats> {
    validate_date(date)?;
    let file_hash = hash_file(input)?;
    let ledger_path = ledger_path(archive_root, date, channel, &file_hash)?;
    if ledger_path.exists() && !force {
        let data = std::fs::read(&ledger_path)?;
        let ledger: ImportLedger = serde_json::from_slice(&data)?;
        return Ok(ImportStats {
            rows: ledger.rows,
            segments: ledger.segments,
            min_ts_ns: ledger.min_ts_ns,
            max_ts_ns: ledger.max_ts_ns,
            channel: ledger.channel,
        });
    }

    let reader = open_input(input)?;
    let mut csv = ReaderBuilder::new()
        .trim(Trim::All)
        .flexible(true)
        .from_reader(reader);
    let headers = csv.headers()?.clone();
    let indices = ColumnIndices::from_headers(&headers)?;

    let mut stats = ImportStats {
        rows: 0,
        segments: 0,
        min_ts_ns: 0,
        max_ts_ns: 0,
        channel: 0,
    };

    let mut expected_seq: Option<u64> = None;
    let mut seen_channel: Option<u32> = None;
    let mut symbols: BTreeSet<String> = BTreeSet::new();

    let mut writer: Option<ArchiveWriter> = None;

    for record in csv.records() {
        let record = record?;
        let row = parse_row(&record, &indices)?;

        let row_channel = row.channel;
        if let Some(expected_channel) = channel {
            if row_channel != expected_channel {
                return Err(anyhow!(
                    "channel mismatch: expected {expected_channel} but saw {row_channel}"
                ));
            }
        }
        if let Some(seen) = seen_channel {
            if seen != row_channel {
                return Err(anyhow!(
                    "multiple ChannelNo values in input: {seen} vs {row_channel}"
                ));
            }
        } else {
            seen_channel = Some(row_channel);
            stats.channel = row_channel;
            let stream_dir = szse_channel_dir(archive_root, date, row_channel)?;
            let mut w = ArchiveWriter::new_at_dir(stream_dir, segment_size)?;
            writer = Some(w);
        }

        let seq = row.seq;
        if let Some(expected) = expected_seq {
            if seq != expected {
                return Err(anyhow!("sequence gap: expected {expected} but saw {seq}"));
            }
        }
        expected_seq = Some(seq.saturating_add(1));

        let payload = encode_l3_event(&row)?;
        let exchange_ts_ns = row.exchange_ts_ns;
        let writer = writer
            .as_mut()
            .ok_or_else(|| anyhow!("writer not initialized"))?;
        writer.append_record(TypeId::BookEvent.as_u16(), exchange_ts_ns, &payload)?;

        stats.rows = stats.rows.saturating_add(1);
        if stats.min_ts_ns == 0 || exchange_ts_ns < stats.min_ts_ns {
            stats.min_ts_ns = exchange_ts_ns;
        }
        if exchange_ts_ns > stats.max_ts_ns {
            stats.max_ts_ns = exchange_ts_ns;
        }
        symbols.insert(row.symbol.clone());
    }

    let writer = writer.ok_or_else(|| anyhow!("no rows in input"))?;
    let mut writer = writer;
    writer.finish()?;
    stats.segments = writer.segments_written();

    let event_time_range = if stats.min_ts_ns > 0 && stats.max_ts_ns > 0 {
        Some([stats.min_ts_ns, stats.max_ts_ns])
    } else {
        None
    };

    let meta = MetaFile {
        symbol_code: Some("MULTI".to_string()),
        venue: Some(VENUE.to_string()),
        ingest_time_ns: now_ns(),
        event_time_range,
        completeness: "sealed".to_string(),
    };
    let meta_path = szse_channel_dir(archive_root, date, stats.channel)?.join("meta.json");
    write_meta_at_if_missing(&meta_path, &meta)?;

    if compress {
        let stream_dir = szse_channel_dir(archive_root, date, stats.channel)?;
        let tier = TierManager::new(TierConfig::new(stream_dir));
        tier.run_once()?;
    }

    write_ledger(
        &ledger_path,
        input,
        &file_hash,
        date,
        stats.channel,
        &stats,
    )?;

    update_channel_index(archive_root, date, stats.channel, symbols)?;

    Ok(stats)
}

#[derive(Debug)]
struct ParsedRow {
    channel: u32,
    seq: u64,
    event_type: L3EventType,
    symbol: String,
    order_id: u64,
    bid_order_id: u64,
    ask_order_id: u64,
    side: L3Side,
    price: u64,
    qty: u64,
    amt: u64,
    ord_type: u8,
    exec_type: u8,
    exchange_ts_ns: u64,
    ingest_ts_ns: u64,
    flags: u32,
    market_id: u32,
}

fn parse_row(record: &StringRecord, indices: &ColumnIndices) -> Result<ParsedRow> {
    let channel = parse_u32(field(record, indices.channel, "ChannelNo")?)?;
    let seq = parse_u64(field(record, indices.seq, "ApplSeqNum")?)?;
    let event_str = field(record, indices.event, "Event")?;
    let event_type = match event_str {
        "ORDER" => L3EventType::OrderAdd,
        "CANCEL" => L3EventType::OrderCancel,
        "TRADE" => L3EventType::Trade,
        other => return Err(anyhow!("unsupported Event type: {other}")),
    };
    let symbol = field(record, indices.symbol, "Symbol")?.to_string();
    let market_id = parse_u32(&symbol)
        .map_err(|_| anyhow!("invalid numeric symbol for market_id: {symbol}"))?;
    let order_id = parse_u64_opt(field(record, indices.order_id, "OrderID")?);
    let bid_order_id = parse_u64_opt(field(record, indices.bid_order_id, "BidOrderID")?);
    let ask_order_id = parse_u64_opt(field(record, indices.ask_order_id, "OfferOrderID")?);
    let side = parse_side(field(record, indices.side, "Side")?)?;
    let price = parse_fixed(field(record, indices.price, "Price")?, PRICE_SCALE);
    let qty = parse_fixed(field(record, indices.qty, "Qty")?, SIZE_SCALE);
    let amt = parse_fixed(field(record, indices.amt, "Amt")?, PRICE_SCALE);
    let ord_type = parse_code(field(record, indices.ord_type, "OrdType")?);
    let exec_type = parse_code(field(record, indices.exec_type, "ExecType")?);
    let transact_time = field(record, indices.transact_time, "TransactTime")?;
    let sending_time = field(record, indices.sending_time, "SendingTime")?;
    let exchange_ts_ns = parse_ts_utc_ns(transact_time)?;
    let ingest_ts_ns = match parse_ts_utc_ns_opt(sending_time)? {
        Some(ts) => ts,
        None => exchange_ts_ns,
    };

    let mut flags = 0u32;
    if price == 0 || ord_type == 1 {
        flags |= l3_flags::PRICE_IS_MARKET;
    }
    if bid_order_id != 0 {
        flags |= l3_flags::RAW_HAS_BID_ID;
    }
    if ask_order_id != 0 {
        flags |= l3_flags::RAW_HAS_ASK_ID;
    }

    if let Some(local) = parse_ts_local(transact_time)? {
        flags |= auction_flags(local);
    }

    let mut order_id = order_id;
    if order_id == 0 && matches!(event_type, L3EventType::OrderCancel) {
        if bid_order_id != 0 && ask_order_id == 0 {
            order_id = bid_order_id;
        } else if ask_order_id != 0 && bid_order_id == 0 {
            order_id = ask_order_id;
        }
    }

    Ok(ParsedRow {
        channel,
        seq,
        event_type,
        symbol,
        order_id,
        bid_order_id,
        ask_order_id,
        side,
        price,
        qty,
        amt,
        ord_type,
        exec_type,
        exchange_ts_ns,
        ingest_ts_ns,
        flags,
        market_id,
    })
}

fn encode_l3_event(row: &ParsedRow) -> Result<Vec<u8>> {
    let record_len = book_event_len::<L3Event>()?;
    let header = BookEventHeader {
        schema_version: PROTOCOL_VERSION,
        record_len,
        endianness: 0,
        _pad0: 0,
        venue_id: 0,
        market_id: row.market_id,
        stream_id: row.channel,
        ingest_ts_ns: row.ingest_ts_ns,
        exchange_ts_ns: row.exchange_ts_ns,
        seq: row.seq,
        native_seq: row.seq,
        event_type: BookEventType::Diff as u8,
        book_mode: BookMode::L3 as u8,
        flags: 0,
        _pad1: 0,
    };
    let payload = L3Event {
        event_type: row.event_type as u8,
        side: row.side as u8,
        ord_type: row.ord_type,
        exec_type: row.exec_type,
        price_scale: PRICE_SCALE,
        size_scale: SIZE_SCALE,
        _pad0: 0,
        flags: row.flags,
        _pad1: 0,
        order_id: row.order_id,
        bid_order_id: row.bid_order_id,
        ask_order_id: row.ask_order_id,
        price: row.price,
        qty: row.qty,
        amt: row.amt,
    };

    let total_len = std::mem::size_of::<BookEventHeader>() + std::mem::size_of::<L3Event>();
    let mut buf = vec![0u8; total_len];
    let mut offset = 0;
    write_struct(&mut buf, offset, &header);
    offset += std::mem::size_of::<BookEventHeader>();
    write_struct(&mut buf, offset, &payload);
    Ok(buf)
}

fn book_event_len<T>() -> Result<u32> {
    let total = std::mem::size_of::<BookEventHeader>() + std::mem::size_of::<T>();
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

fn auction_flags(local: LocalTimestamp) -> u32 {
    let hhmm = local.hour as u32 * 100 + local.minute as u32;
    let mut flags = 0u32;
    if (915..=925).contains(&hhmm) {
        flags |= l3_flags::AUCTION_OPEN;
        if (920..=925).contains(&hhmm) {
            flags |= l3_flags::CANCEL_RESTRICTED_WINDOW;
        }
    }
    if (1457..=1500).contains(&hhmm) {
        flags |= l3_flags::AUCTION_CLOSE;
        flags |= l3_flags::CANCEL_RESTRICTED_WINDOW;
    }
    flags
}

#[derive(Debug, Clone, Copy)]
struct LocalTimestamp {
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    millisecond: u16,
}

fn parse_ts_local(value: &str) -> Result<Option<LocalTimestamp>> {
    let value = value.trim();
    if value.is_empty() || value == "0" {
        return Ok(None);
    }
    if value.len() != 17 {
        return Err(anyhow!("invalid timestamp length: {value}"));
    }
    let year = value[0..4].parse::<i32>()?;
    let month = value[4..6].parse::<u8>()?;
    let day = value[6..8].parse::<u8>()?;
    let hour = value[8..10].parse::<u8>()?;
    let minute = value[10..12].parse::<u8>()?;
    let second = value[12..14].parse::<u8>()?;
    let millisecond = value[14..17].parse::<u16>()?;
    Ok(Some(LocalTimestamp {
        year,
        month,
        day,
        hour,
        minute,
        second,
        millisecond,
    }))
}

fn parse_ts_utc_ns(value: &str) -> Result<u64> {
    parse_ts_utc_ns_opt(value)?.ok_or_else(|| anyhow!("missing timestamp"))
}

fn parse_ts_utc_ns_opt(value: &str) -> Result<Option<u64>> {
    let Some(local) = parse_ts_local(value)? else {
        return Ok(None);
    };
    let month = Month::try_from(local.month).map_err(|_| anyhow!("invalid month"))?;
    let date = Date::from_calendar_date(local.year, month, local.day)
        .map_err(|_| anyhow!("invalid date"))?;
    let time = Time::from_hms_milli(local.hour, local.minute, local.second, local.millisecond)
        .map_err(|_| anyhow!("invalid time"))?;
    let pdt = PrimitiveDateTime::new(date, time);
    let offset = UtcOffset::from_hms(8, 0, 0).map_err(|_| anyhow!("invalid offset"))?;
    let odt = pdt.assume_offset(offset).to_offset(UtcOffset::UTC);
    let ns = odt.unix_timestamp_nanos();
    if ns < 0 {
        return Err(anyhow!("timestamp before unix epoch"));
    }
    Ok(Some(ns as u64))
}

fn parse_side(value: &str) -> Result<L3Side> {
    match value.trim() {
        "" => Ok(L3Side::Unknown),
        "1" => Ok(L3Side::Buy),
        "2" => Ok(L3Side::Sell),
        other => Err(anyhow!("invalid side: {other}")),
    }
}

fn parse_code(value: &str) -> u8 {
    let value = value.trim();
    if value.is_empty() {
        return 0;
    }
    if value.len() == 1 {
        let b = value.as_bytes()[0];
        if b.is_ascii_digit() {
            return (b - b'0') as u8;
        }
        return b;
    }
    value.parse::<u8>().unwrap_or(0)
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

fn parse_u32(value: &str) -> Result<u32> {
    value
        .parse::<u32>()
        .map_err(|_| anyhow!("invalid u32: {value}"))
}

fn parse_u64_opt(value: &str) -> u64 {
    value.trim().parse::<u64>().unwrap_or(0)
}

fn validate_date(date: &str) -> Result<()> {
    if date.len() != 10 {
        return Err(anyhow!("invalid date format (expected YYYY-MM-DD): {date}"));
    }
    let bytes = date.as_bytes();
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return Err(anyhow!("invalid date format (expected YYYY-MM-DD): {date}"));
    }
    for (idx, byte) in bytes.iter().enumerate() {
        if idx == 4 || idx == 7 {
            continue;
        }
        if !byte.is_ascii_digit() {
            return Err(anyhow!("invalid date format (expected YYYY-MM-DD): {date}"));
        }
    }
    Ok(())
}

fn open_input(path: &Path) -> Result<Box<dyn Read>> {
    let file = File::open(path).with_context(|| format!("open input {}", path.display()))?;
    Ok(Box::new(file))
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
    date: &str,
    channel: Option<u32>,
    file_hash: &str,
) -> Result<PathBuf> {
    let channel_name = channel.map(|c| c.to_string()).unwrap_or_else(|| "unknown".to_string());
    let dir = archive_root
        .join("imports")
        .join("v1")
        .join("szse_l3")
        .join(date)
        .join(channel_name);
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{file_hash}.json")))
}

fn write_ledger(
    path: &Path,
    input: &Path,
    file_hash: &str,
    date: &str,
    channel: u32,
    stats: &ImportStats,
) -> Result<()> {
    let ledger = ImportLedger {
        source_path: input.display().to_string(),
        file_hash: file_hash.to_string(),
        rows: stats.rows,
        segments: stats.segments,
        min_ts_ns: stats.min_ts_ns,
        max_ts_ns: stats.max_ts_ns,
        imported_at_ns: now_ns(),
        venue: VENUE.to_string(),
        date: date.to_string(),
        channel,
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

fn szse_channel_dir(root: &Path, date: &str, channel: u32) -> Result<PathBuf> {
    validate_date(date)?;
    let dir = root
        .join(ARCHIVE_VERSION)
        .join(VENUE)
        .join(date)
        .join(channel.to_string());
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn update_channel_index(
    archive_root: &Path,
    date: &str,
    channel: u32,
    symbols: BTreeSet<String>,
) -> Result<()> {
    let index_path = archive_root
        .join(ARCHIVE_VERSION)
        .join(VENUE)
        .join(date)
        .join("channel_index.json");
    let mut index = if index_path.exists() {
        let data = std::fs::read(&index_path)?;
        serde_json::from_slice::<ChannelIndex>(&data)?
    } else {
        ChannelIndex {
            date: date.to_string(),
            ..Default::default()
        }
    };

    let channel_key = channel.to_string();
    let entry = index
        .channel_to_symbols
        .entry(channel_key.clone())
        .or_default();
    for symbol in symbols {
        if let Some(existing) = index.symbol_to_channel.get(&symbol) {
            if *existing != channel {
                return Err(anyhow!(
                    "symbol {symbol} mapped to channel {existing} but now saw {channel}"
                ));
            }
        } else {
            index.symbol_to_channel.insert(symbol.clone(), channel);
        }
        entry.push(symbol);
    }
    entry.sort();
    entry.dedup();

    index.updated_at_ns = now_ns();

    let data = serde_json::to_vec_pretty(&index)?;
    let tmp = index_path.with_extension("json.tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(tmp, index_path)?;
    Ok(())
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
