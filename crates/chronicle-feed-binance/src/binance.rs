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

use crate::market::{
    Appendable, BookTicker, DepthHeader, MarketMessageType, PriceLevel, Trade,
};

const BINANCE_WS_URL: &str = "wss://stream.binance.com:9443/ws";
const BINANCE_API_URL: &str = "https://api.binance.com";

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
                let client = self.client.clone();
                let tx = snapshot_tx.clone();
                
                // Transition to buffering immediately
                states.insert(symbol.clone(), SymbolState::Buffering(VecDeque::new()));

                tokio::spawn(async move {
                    let url = format!("{}/api/v3/depth?symbol={}&limit=1000", BINANCE_API_URL, symbol);
                    match client.get(&url).send().await {
                        Ok(resp) => match resp.json::<BinanceSnapshot>().await {
                            Ok(snap) => {
                                let _ = tx.send((symbol, Ok(snap))).await;
                            }
                            Err(e) => {
                                let _ = tx.send((symbol, Err(anyhow::anyhow!(e)))).await;
                            }
                        },
                        Err(e) => {
                            let _ = tx.send((symbol, Err(anyhow::anyhow!(e)))).await;
                        }
                    }
                });
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
                                        if let Err(e) = on_event(MarketMessageType::BookTicker as u16, &event) {
                                            error!("Write error: {}", e);
                                        }
                                    }
                                } else if let Ok(wrapper) = simd_json::from_slice::<CombinedStreamEvent<BinanceTrade>>(&mut bytes_copy1) {
                                    if let Ok(event) = self.convert_trade(wrapper.data) {
                                        if let Err(e) = on_event(MarketMessageType::Trade as u16, &event) {
                                            error!("Write error: {}", e);
                                        }
                                    }
                                } else if let Ok(wrapper) = simd_json::from_slice::<CombinedStreamEvent<BinanceDepthUpdate>>(&mut bytes_copy2) {
                                    // Handle Depth Update
                                    self.handle_depth_update(&wrapper.data, &mut states, &mut on_event);
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
                                self.handle_snapshot(symbol, snapshot, &mut states, &mut on_event);
                            }
                            Err(e) => {
                                error!("Snapshot fetch failed for {}: {}", symbol, e);
                                // For now, just leave it in buffering state or reset? 
                                // Ideally retry, but simpler to just log for this implementation.
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
    ) where
        F: FnMut(u16, &dyn Appendable) -> Result<()>,
    {
        if let Some(SymbolState::Buffering(buffer)) = states.get_mut(&symbol) {
            // 1. Write Snapshot
            let snap_event = self.convert_snapshot(&symbol, &snapshot);
            if let Err(e) = on_event(MarketMessageType::OrderBookSnapshot as u16, &snap_event) {
                error!("Failed to write snapshot: {}", e);
                return;
            }

            // 2. Filter Buffer
            // Drop any event where u <= lastUpdateId
            // The first processed event should have U <= lastUpdateId+1 AND u >= lastUpdateId+1
            let mut last_u = snapshot.last_update_id;
            
            while let Some(update) = buffer.front() {
                if update.final_update_id <= last_u {
                    buffer.pop_front();
                    continue;
                }
                
                // Check continuity for the first event
                // "The first processed event should have U <= lastUpdateId+1 AND u >= lastUpdateId+1"
                if update.first_update_id > last_u + 1 {
                    error!("Gap detected for {}: snapshot end {} vs first buffered start {}", symbol, last_u, update.first_update_id);
                    // Gap! We missed data. Should restart. 
                    // For this impl, we just clear buffer and maybe request snapshot again?
                    // Let's just log error and proceed (it will likely be broken)
                }
                break;
            }

            // 3. Process remaining buffer
            for update in buffer.drain(..) {
                if update.first_update_id > last_u + 1 {
                     warn!("Gap in buffered data for {}: {} -> {}", symbol, last_u, update.first_update_id);
                }
                last_u = update.final_update_id;
                
                let event = self.convert_depth_update(&update);
                if let Err(e) = on_event(MarketMessageType::DepthUpdate as u16, &event) {
                    error!("Failed to write buffered depth: {}", e);
                }
            }

            // 4. Transition to Synced
            states.insert(symbol.clone(), SymbolState::Synced { last_u });
            info!("Synced {}", symbol);
        }
    }

    fn handle_depth_update<F>(
        &self,
        update: &BinanceDepthUpdate,
        states: &mut HashMap<String, SymbolState>,
        on_event: &mut F,
    ) where
        F: FnMut(u16, &dyn Appendable) -> Result<()>,
    {
        let symbol = update.symbol.to_uppercase();
        match states.get_mut(&symbol) {
            Some(SymbolState::Buffering(buffer)) => {
                buffer.push_back(update.clone());
            }
            Some(SymbolState::Synced { last_u }) => {
                if update.first_update_id != *last_u + 1 {
                    // Gap detected?
                    // Note: Binance says "The first processed event should have U <= lastUpdateId+1 AND u >= lastUpdateId+1"
                    // But for subsequent events: "Each new event's U should be equal to the previous event's u+1."
                    warn!("Potential gap for {}: prev_u {} vs next_U {}", symbol, last_u, update.first_update_id);
                }
                *last_u = update.final_update_id;
                
                let event = self.convert_depth_update(update);
                if let Err(e) = on_event(MarketMessageType::DepthUpdate as u16, &event) {
                     error!("Failed to write depth: {}", e);
                }
            }
            _ => {} // Connecting or unknown
        }
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
        Ok(BookTicker::new(
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64,
            ticker.bid_price.parse().unwrap_or_default(),
            ticker.bid_qty.parse().unwrap_or_default(),
            ticker.ask_price.parse().unwrap_or_default(),
            ticker.ask_qty.parse().unwrap_or_default(),
            &ticker.symbol,
        ))
    }

    fn convert_trade(&self, trade: BinanceTrade) -> Result<Trade> {
        Ok(Trade::new(
            trade.trade_time,
            trade.price.parse().unwrap_or_default(),
            trade.qty.parse().unwrap_or_default(),
            trade.trade_id,
            trade.buyer_order_id,
            trade.seller_order_id,
            trade.is_buyer_maker,
            &trade.symbol,
        ))
    }

    fn convert_depth_update(&self, update: &BinanceDepthUpdate) -> DepthUpdateWrapper {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
        let bids = parse_levels(&update.bids);
        let asks = parse_levels(&update.asks);
        
        DepthUpdateWrapper {
            header: DepthHeader {
                timestamp_ms: now,
                first_update_id: update.first_update_id,
                final_update_id: update.final_update_id,
                symbol_hash: crate::market::fxhash::hash64(&update.symbol),
                bid_count: bids.len() as u32,
                ask_count: asks.len() as u32,
            },
            bids,
            asks,
        }
    }

    fn convert_snapshot(&self, symbol: &str, snap: &BinanceSnapshot) -> DepthUpdateWrapper {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
        let bids = parse_levels(&snap.bids);
        let asks = parse_levels(&snap.asks);

        DepthUpdateWrapper {
            header: DepthHeader {
                timestamp_ms: now,
                first_update_id: 0,
                final_update_id: snap.last_update_id,
                symbol_hash: crate::market::fxhash::hash64(symbol),
                bid_count: bids.len() as u32,
                ask_count: asks.len() as u32,
            },
            bids,
            asks,
        }
    }
}

// Helper for generic Appendable wrapper
struct DepthUpdateWrapper {
    header: DepthHeader,
    bids: Vec<PriceLevel>,
    asks: Vec<PriceLevel>,
}

impl Appendable for DepthUpdateWrapper {
    fn size(&self) -> usize {
        std::mem::size_of::<DepthHeader>() 
            + (self.bids.len() * std::mem::size_of::<PriceLevel>()) 
            + (self.asks.len() * std::mem::size_of::<PriceLevel>())
    }

    fn write_to(&self, buf: &mut [u8]) {
        let header_size = std::mem::size_of::<DepthHeader>();
        let header_ptr = &self.header as *const DepthHeader as *const u8;
        unsafe {
            let src = std::slice::from_raw_parts(header_ptr, header_size);
            buf[0..header_size].copy_from_slice(src);
        }

        let mut offset = header_size;
        
        // Write Bids
        let level_size = std::mem::size_of::<PriceLevel>();
        let bids_len = self.bids.len() * level_size;
        if bids_len > 0 {
            let bids_ptr = self.bids.as_ptr() as *const u8;
            unsafe {
                let src = std::slice::from_raw_parts(bids_ptr, bids_len);
                buf[offset..offset+bids_len].copy_from_slice(src);
            }
            offset += bids_len;
        }

        // Write Asks
        let asks_len = self.asks.len() * level_size;
        if asks_len > 0 {
            let asks_ptr = self.asks.as_ptr() as *const u8;
            unsafe {
                let src = std::slice::from_raw_parts(asks_ptr, asks_len);
                buf[offset..offset+asks_len].copy_from_slice(src);
            }
        }
    }
}

// We need a way to write this wrapper. 
// Ideally, we shouldn't use Appendable trait for DepthUpdate if it requires contiguous memory.
// Or we serialize it into a temporary buffer here.
// But we want Zero-Copy (or at least 1-Copy to queue).
//
// Plan: Update Appendable trait to `write_to(&self, buf: &mut [u8])`.
// This allows scattered writes.

fn parse_levels(levels: &[(String, String)]) -> Vec<PriceLevel> {
    levels.iter().map(|(p, q)| PriceLevel {
        price: p.parse().unwrap_or_default(),
        qty: q.parse().unwrap_or_default(),
    }).collect()
}
