use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use log::{error, info, warn};
use serde::Deserialize;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use url::Url;

use crate::market::{fxhash, market_id, Appendable};
use chronicle_protocol::{
    book_flags, BookEventHeader, BookEventType, BookMode, BookTicker, L2Diff, L2Snapshot,
    PriceLevelUpdate, Trade, TypeId, PROTOCOL_VERSION,
};

const BINANCE_WS_URL: &str = "wss://stream.binance.com:9443/ws";
const BINANCE_API_URL: &str = "https://api.binance.com";
const SNAPSHOT_RETRY_DELAY: Duration = Duration::from_secs(2);

#[derive(Deserialize, Debug)]
struct BinanceBookTicker {
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "b")]
    bid_price: String,
    #[serde(rename = "B")]
    bid_qty: String,
    #[serde(rename = "a")]
    ask_price: String,
    #[serde(rename = "A")]
    ask_qty: String,
}

#[derive(Deserialize, Debug)]
struct BinanceTrade {
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "t")]
    trade_id: u64,
    #[serde(rename = "p")]
    price: String,
    #[serde(rename = "q")]
    qty: String,
    #[serde(rename = "b")]
    buyer_order_id: u64,
    #[serde(rename = "a")]
    seller_order_id: u64,
    #[serde(rename = "T")]
    trade_time: u64,
    #[serde(rename = "m")]
    is_buyer_maker: bool,
}

#[derive(Deserialize, Debug, Clone)]
struct BinanceDepthUpdate {
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "U")]
    first_update_id: u64,
    #[serde(rename = "u")]
    final_update_id: u64,
    #[serde(rename = "b")]
    bids: Vec<(String, String)>,
    #[serde(rename = "a")]
    asks: Vec<(String, String)>,
}

#[derive(Deserialize, Debug)]
struct BinanceSnapshot {
    #[serde(rename = "lastUpdateId")]
    last_update_id: u64,
    bids: Vec<(String, String)>,
    asks: Vec<(String, String)>,
}

// For combined streams
#[derive(Deserialize, Debug)]
struct CombinedStreamEvent<T> {
    #[allow(dead_code)]
    stream: String,
    data: T,
}

enum SymbolState {
    // Waiting for WS connection to establish flow
    Connecting,
    // WS connected, buffering events, fetching snapshot
    Buffering(VecDeque<BinanceDepthUpdate>),
    // Snapshot applied, streaming diffs
    Synced { last_u: u64 },
}

pub struct BinanceFeed {
    symbols: Vec<String>,
    url: Url,
    client: reqwest::Client,
}

impl BinanceFeed {
    pub fn new(symbols: &[String]) -> Self {
        Self {
            symbols: symbols.to_vec(),
            url: Url::parse(BINANCE_WS_URL).expect("Invalid hardcoded URL"),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("Failed to build reqwest client"),
        }
    }

    pub async fn run<F>(self, mut on_event: F) -> Result<()>
    where
        F: FnMut(u16, &dyn Appendable) -> Result<()>,
    {
        loop {
            info!("Connecting to Binance WS...");
            let mut ws_stream = match self.connect_and_subscribe().await {
                Ok(res) => res,
                Err(e) => {
                    error!("Connection failed: {}. Retrying in 5s...", e);
                    sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };
            info!("Connected.");

            // Reset state for all symbols
            let mut states: HashMap<String, SymbolState> = self
                .symbols
                .iter()
                .map(|s| (s.to_uppercase(), SymbolState::Connecting))
                .collect();

            // Channel for snapshot results
            let (snapshot_tx, mut snapshot_rx) = mpsc::channel(100);

            // Trigger snapshot fetches for all symbols
            for symbol in &self.symbols {
                let symbol = symbol.to_uppercase();
                
                // Transition to buffering immediately
                states.insert(symbol.clone(), SymbolState::Buffering(VecDeque::new()));
                self.request_snapshot(&symbol, &snapshot_tx, Duration::ZERO);
            }

            // Main Event Loop
            loop {
                tokio::select! {
                    Some(msg) = ws_stream.next() => {
                        match msg {
                            Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                                let mut bytes = text.into_bytes();
                                // We clone bytes for fallbacks because simd-json mutates them.
                                let mut bytes_copy1 = bytes.clone();
                                let mut bytes_copy2 = bytes.clone();

                                if let Ok(wrapper) = simd_json::from_slice::<CombinedStreamEvent<BinanceBookTicker>>(&mut bytes) {
                                    if let Ok(event) = self.convert_ticker(wrapper.data) {
                                        if let Err(e) = on_event(TypeId::BookTicker.as_u16(), &event) {
                                            error!("Write error: {}", e);
                                        }
                                    }
                                } else if let Ok(wrapper) = simd_json::from_slice::<CombinedStreamEvent<BinanceTrade>>(&mut bytes_copy1) {
                                    if let Ok(event) = self.convert_trade(wrapper.data) {
                                        if let Err(e) = on_event(TypeId::Trade.as_u16(), &event) {
                                            error!("Write error: {}", e);
                                        }
                                    }
                                } else if let Ok(wrapper) = simd_json::from_slice::<CombinedStreamEvent<BinanceDepthUpdate>>(&mut bytes_copy2) {
                                    // Handle Depth Update
                                    self.handle_depth_update(&wrapper.data, &mut states, &mut on_event, &snapshot_tx);
                                }
                            }
                            Ok(tokio_tungstenite::tungstenite::Message::Ping(ping)) => {
                                let _ = ws_stream.send(tokio_tungstenite::tungstenite::Message::Pong(ping)).await;
                            }
                            Err(e) => {
                                error!("WS Error: {}", e);
                                break; 
                            }
                            _ => {}
                        }
                    }
                    Some((symbol, res)) = snapshot_rx.recv() => {
                        match res {
                            Ok(snapshot) => {
                                info!("Snapshot received for {}", symbol);
                                self.handle_snapshot(symbol, snapshot, &mut states, &mut on_event, &snapshot_tx);
                            }
                            Err(e) => {
                                error!("Snapshot fetch failed for {}: {}", symbol, e);
                                if let Some(state) = states.remove(&symbol) {
                                    match state {
                                        SymbolState::Buffering(mut buffer) => {
                                            buffer.clear();
                                            states.insert(symbol.clone(), SymbolState::Buffering(buffer));
                                            self.request_snapshot(&symbol, &snapshot_tx, SNAPSHOT_RETRY_DELAY);
                                        }
                                        other => {
                                            states.insert(symbol, other);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    else => break, // Channel closed or stream ended
                }
            }
            
            warn!("Disconnected or error. Reconnecting...");
            sleep(Duration::from_secs(5)).await;
        }
    }

    fn handle_snapshot<F>(
        &self,
        symbol: String,
        snapshot: BinanceSnapshot,
        states: &mut HashMap<String, SymbolState>,
        on_event: &mut F,
        snapshot_tx: &mpsc::Sender<(String, Result<BinanceSnapshot>)>,
    ) where
        F: FnMut(u16, &dyn Appendable) -> Result<()>,
    {
        if let Some(state) = states.remove(&symbol) {
            match state {
                SymbolState::Buffering(mut buffer) => {
                    let mut last_u = snapshot.last_update_id;

                    // Drop any event where u <= lastUpdateId
                    while let Some(update) = buffer.front() {
                        if update.final_update_id <= last_u {
                            buffer.pop_front();
                        } else {
                            break;
                        }
                    }

                    // Validate continuity before publishing snapshot.
                    let mut gap_detected = None;
                    if let Some(first) = buffer.front() {
                        if first.first_update_id > last_u + 1 {
                            gap_detected = Some((last_u, first.first_update_id));
                        } else {
                            let mut expected_last_u = first.final_update_id;
                            for update in buffer.iter().skip(1) {
                                if update.first_update_id != expected_last_u + 1 {
                                    gap_detected = Some((expected_last_u, update.first_update_id));
                                    break;
                                }
                                expected_last_u = update.final_update_id;
                            }
                        }
                    }

                    if let Some((prev_u, next_u)) = gap_detected {
                        warn!(
                            "Gap detected for {} during snapshot replay: prev_u {} vs next_U {}. Resyncing.",
                            symbol, prev_u, next_u
                        );
                        states.insert(symbol.clone(), SymbolState::Buffering(buffer));
                        self.request_snapshot(&symbol, snapshot_tx, SNAPSHOT_RETRY_DELAY);
                        return;
                    }

                    // 1. Write Snapshot
                    let snap_event = match self.convert_snapshot(&symbol, &snapshot) {
                        Ok(event) => event,
                        Err(e) => {
                            error!("Failed to build snapshot event: {}", e);
                            states.insert(symbol, SymbolState::Buffering(buffer));
                            return;
                        }
                    };
                    if let Err(e) = on_event(TypeId::BookEvent.as_u16(), &snap_event) {
                        error!("Failed to write snapshot: {}", e);
                        states.insert(symbol, SymbolState::Buffering(buffer));
                        return;
                    }

                    // 2. Process remaining buffer
                    for update in buffer.drain(..) {
                        last_u = update.final_update_id;
                        match self.convert_depth_update(&update) {
                            Ok(event) => {
                                if let Err(e) = on_event(TypeId::BookEvent.as_u16(), &event) {
                                    error!("Failed to write buffered depth: {}", e);
                                }
                            }
                            Err(e) => {
                                error!("Failed to build buffered depth event: {}", e);
                            }
                        }
                    }

                    // 3. Transition to Synced
                    states.insert(symbol.clone(), SymbolState::Synced { last_u });
                    info!("Synced {}", symbol);
                }
                other => {
                    states.insert(symbol, other);
                }
            }
        }
    }

    fn handle_depth_update<F>(
        &self,
        update: &BinanceDepthUpdate,
        states: &mut HashMap<String, SymbolState>,
        on_event: &mut F,
        snapshot_tx: &mpsc::Sender<(String, Result<BinanceSnapshot>)>,
    ) where
        F: FnMut(u16, &dyn Appendable) -> Result<()>,
    {
        let symbol = update.symbol.to_uppercase();
        if let Some(state) = states.remove(&symbol) {
            match state {
                SymbolState::Buffering(mut buffer) => {
                    buffer.push_back(update.clone());
                    states.insert(symbol, SymbolState::Buffering(buffer));
                }
                SymbolState::Synced { last_u } => {
                    if update.first_update_id != last_u + 1 {
                        warn!(
                            "Gap detected for {}: prev_u {} vs next_U {}. Resyncing.",
                            symbol, last_u, update.first_update_id
                        );
                        let mut buffer = VecDeque::new();
                        buffer.push_back(update.clone());
                        states.insert(symbol.clone(), SymbolState::Buffering(buffer));
                        self.request_snapshot(&symbol, snapshot_tx, SNAPSHOT_RETRY_DELAY);
                        return;
                    }

                    match self.convert_depth_update(update) {
                        Ok(event) => {
                            if let Err(e) = on_event(TypeId::BookEvent.as_u16(), &event) {
                                error!("Failed to write depth: {}", e);
                            }
                        }
                        Err(e) => {
                            error!("Failed to build depth event: {}", e);
                        }
                    }
                    states.insert(symbol, SymbolState::Synced { last_u: update.final_update_id });
                }
                other => {
                    states.insert(symbol, other);
                }
            }
        }
    }

    fn request_snapshot(
        &self,
        symbol: &str,
        snapshot_tx: &mpsc::Sender<(String, Result<BinanceSnapshot>)>,
        delay: Duration,
    ) {
        let symbol = symbol.to_uppercase();
        let client = self.client.clone();
        let tx = snapshot_tx.clone();
        tokio::spawn(async move {
            if !delay.is_zero() {
                sleep(delay).await;
            }
            let url = format!("{}/api/v3/depth?symbol={}&limit=1000", BINANCE_API_URL, symbol);
            let result: Result<BinanceSnapshot> = async {
                let resp = client.get(&url).send().await?;
                let resp = resp.error_for_status()?;
                let snapshot = resp.json::<BinanceSnapshot>().await?;
                Ok(snapshot)
            }
            .await;
            let _ = tx.send((symbol, result)).await;
        });
    }

    async fn connect_and_subscribe(&self) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
        let mut streams = Vec::new();
        for s in &self.symbols {
            streams.push(format!("{}@bookTicker", s.to_lowercase()));
            streams.push(format!("{}@trade", s.to_lowercase()));
            streams.push(format!("{}@depth@100ms", s.to_lowercase()));
        }
        
        let query = format!("streams={}", streams.join("/"));
        let mut url = self.url.clone();
        url.set_path("stream");
        url.set_query(Some(&query));

        let (ws_stream, _) = connect_async(url).await.context("Failed to connect")?;
        Ok(ws_stream)
    }

    // --- Converters ---

    fn convert_ticker(&self, ticker: BinanceBookTicker) -> Result<BookTicker> {
        let now_ns = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
        Ok(BookTicker {
            timestamp_ns: now_ns,
            bid_price: ticker.bid_price.parse().unwrap_or_default(),
            bid_qty: ticker.bid_qty.parse().unwrap_or_default(),
            ask_price: ticker.ask_price.parse().unwrap_or_default(),
            ask_qty: ticker.ask_qty.parse().unwrap_or_default(),
            symbol_hash: fxhash::hash64(&ticker.symbol),
        })
    }

    fn convert_trade(&self, trade: BinanceTrade) -> Result<Trade> {
        Ok(Trade {
            timestamp_ns: trade.trade_time.saturating_mul(1_000_000),
            price: trade.price.parse().unwrap_or_default(),
            qty: trade.qty.parse().unwrap_or_default(),
            trade_id: trade.trade_id,
            buyer_order_id: trade.buyer_order_id,
            seller_order_id: trade.seller_order_id,
            is_buyer_maker: trade.is_buyer_maker,
            _pad0: [0u8; 7],
            symbol_hash: fxhash::hash64(&trade.symbol),
        })
    }

    fn convert_depth_update(&self, update: &BinanceDepthUpdate) -> Result<L2DiffEvent> {
        let now_ns = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
        let (price_scale, size_scale) = max_scales(&update.bids, &update.asks);
        let bids = parse_levels(&update.bids, price_scale, size_scale);
        let asks = parse_levels(&update.asks, price_scale, size_scale);
        let record_len = book_event_len::<L2Diff>(bids.len(), asks.len())?;

        let bid_count = u16::try_from(bids.len())
            .map_err(|_| anyhow::anyhow!("bid_count exceeds u16"))?;
        let ask_count = u16::try_from(asks.len())
            .map_err(|_| anyhow::anyhow!("ask_count exceeds u16"))?;

        Ok(L2DiffEvent {
            header: BookEventHeader {
                schema_version: PROTOCOL_VERSION,
                record_len,
                endianness: 0,
                _pad0: 0,
                venue_id: VENUE_ID_BINANCE,
                market_id: market_id(&update.symbol),
                stream_id: 0,
                ingest_ts_ns: now_ns,
                exchange_ts_ns: now_ns,
                seq: 0,
                native_seq: update.final_update_id,
                event_type: BookEventType::Diff as u8,
                book_mode: BookMode::L2 as u8,
                flags: 0,
                _pad1: 0,
            },
            diff: L2Diff {
                update_id_first: update.first_update_id,
                update_id_last: update.final_update_id,
                update_id_prev: 0,
                price_scale,
                size_scale,
                flags: book_flags::ABSOLUTE,
                bid_count,
                ask_count,
            },
            bids,
            asks,
        })
    }

    fn convert_snapshot(&self, symbol: &str, snap: &BinanceSnapshot) -> Result<L2SnapshotEvent> {
        let now_ns = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
        let (price_scale, size_scale) = max_scales(&snap.bids, &snap.asks);
        let bids = parse_levels(&snap.bids, price_scale, size_scale);
        let asks = parse_levels(&snap.asks, price_scale, size_scale);
        let record_len = book_event_len::<L2Snapshot>(bids.len(), asks.len())?;

        Ok(L2SnapshotEvent {
            header: BookEventHeader {
                schema_version: PROTOCOL_VERSION,
                record_len,
                endianness: 0,
                _pad0: 0,
                venue_id: VENUE_ID_BINANCE,
                market_id: market_id(symbol),
                stream_id: 0,
                ingest_ts_ns: now_ns,
                exchange_ts_ns: now_ns,
                seq: 0,
                native_seq: snap.last_update_id,
                event_type: BookEventType::Snapshot as u8,
                book_mode: BookMode::L2 as u8,
                flags: 0,
                _pad1: 0,
            },
            snapshot: L2Snapshot {
                price_scale,
                size_scale,
                _pad0: 0,
                bid_count: bids.len() as u32,
                ask_count: asks.len() as u32,
            },
            bids,
            asks,
        })
    }
}

const VENUE_ID_BINANCE: u16 = 1;

struct L2DiffEvent {
    header: BookEventHeader,
    diff: L2Diff,
    bids: Vec<PriceLevelUpdate>,
    asks: Vec<PriceLevelUpdate>,
}

struct L2SnapshotEvent {
    header: BookEventHeader,
    snapshot: L2Snapshot,
    bids: Vec<PriceLevelUpdate>,
    asks: Vec<PriceLevelUpdate>,
}

impl Appendable for L2DiffEvent {
    fn size(&self) -> usize {
        std::mem::size_of::<BookEventHeader>()
            + std::mem::size_of::<L2Diff>()
            + (self.bids.len() + self.asks.len()) * std::mem::size_of::<PriceLevelUpdate>()
    }

    fn write_to(&self, buf: &mut [u8]) {
        write_struct(buf, 0, &self.header);
        let mut offset = std::mem::size_of::<BookEventHeader>();
        write_struct(buf, offset, &self.diff);
        offset += std::mem::size_of::<L2Diff>();
        offset = write_levels(buf, offset, &self.bids);
        let _ = write_levels(buf, offset, &self.asks);
    }

    fn timestamp_ns(&self) -> u64 {
        self.header.ingest_ts_ns
    }
}

impl Appendable for L2SnapshotEvent {
    fn size(&self) -> usize {
        std::mem::size_of::<BookEventHeader>()
            + std::mem::size_of::<L2Snapshot>()
            + (self.bids.len() + self.asks.len()) * std::mem::size_of::<PriceLevelUpdate>()
    }

    fn write_to(&self, buf: &mut [u8]) {
        write_struct(buf, 0, &self.header);
        let mut offset = std::mem::size_of::<BookEventHeader>();
        write_struct(buf, offset, &self.snapshot);
        offset += std::mem::size_of::<L2Snapshot>();
        offset = write_levels(buf, offset, &self.bids);
        let _ = write_levels(buf, offset, &self.asks);
    }

    fn timestamp_ns(&self) -> u64 {
        self.header.ingest_ts_ns
    }
}

fn write_struct<T>(buf: &mut [u8], offset: usize, value: &T) {
    let size = std::mem::size_of::<T>();
    let ptr = value as *const T as *const u8;
    unsafe {
        let src = std::slice::from_raw_parts(ptr, size);
        buf[offset..offset + size].copy_from_slice(src);
    }
}

fn write_levels(
    buf: &mut [u8],
    mut offset: usize,
    levels: &[PriceLevelUpdate],
) -> usize {
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

fn book_event_len<T>(bid_len: usize, ask_len: usize) -> Result<u16> {
    let total = std::mem::size_of::<BookEventHeader>()
        + std::mem::size_of::<T>()
        + (bid_len + ask_len) * std::mem::size_of::<PriceLevelUpdate>();
    u16::try_from(total).map_err(|_| anyhow::anyhow!("BookEvent record_len exceeds u16"))
}

fn max_scales(bids: &[(String, String)], asks: &[(String, String)]) -> (u8, u8) {
    let mut price_scale = 0u8;
    let mut size_scale = 0u8;
    for (p, q) in bids.iter().chain(asks.iter()) {
        price_scale = price_scale.max(decimal_scale(p));
        size_scale = size_scale.max(decimal_scale(q));
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

fn parse_levels(
    levels: &[(String, String)],
    price_scale: u8,
    size_scale: u8,
) -> Vec<PriceLevelUpdate> {
    levels
        .iter()
        .map(|(p, q)| PriceLevelUpdate {
            price: parse_fixed(p, price_scale),
            size: parse_fixed(q, size_scale),
        })
        .collect()
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

    let scaled = int_val
        .saturating_mul(pow10 as u128)
        .saturating_add(frac_val.saturating_mul(POW10[(scale as usize).saturating_sub(frac_len)] as u128));
    scaled.min(u64::MAX as u128) as u64
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
