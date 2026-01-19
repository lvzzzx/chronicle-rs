use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use log::{error, info};
use serde::Deserialize;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use url::Url;

use crate::market::{BookTicker, MarketMessageType, Trade, Appendable};

const BINANCE_WS_URL: &str = "wss://stream.binance.com:9443/ws";

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

// For combined streams, the payload is wrapped in {"stream": "...", "data": ...}
#[derive(Deserialize, Debug)]
struct CombinedStreamEvent<T> {
    #[allow(dead_code)]
    stream: String,
    data: T,
}

pub struct BinanceFeed {
    symbols: Vec<String>,
    url: Url,
}

impl BinanceFeed {
    pub fn new(symbols: &[String]) -> Self {
        Self {
            symbols: symbols.to_vec(),
            url: Url::parse(BINANCE_WS_URL).expect("Invalid hardcoded URL"),
        }
    }

    pub async fn run<F>(self, mut on_event: F) -> Result<()>
    where
        F: FnMut(u16, &dyn Appendable) -> Result<()>,
    {
        loop {
            info!("Connecting to Binance WS...");
            match self.connect_and_subscribe().await {
                Ok(mut ws_stream) => {
                    info!("Connected. Processing messages...");
                    while let Some(msg) = ws_stream.next().await {
                        match msg {
                            Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                                let mut bytes = text.into_bytes();
                                
                                // Attempt to parse as CombinedStreamEvent first since we use combined streams
                                // We'll try to determine the type based on structure or trial-and-error
                                // Since simd-json modifies the buffer, we need to be careful.
                                // A robust way: Parse as a generic Value first? No, that's slow.
                                // Check the stream name if we parse as CombinedStreamEvent.
                                // But CombinedStreamEvent<T> needs T known.
                                
                                // Strategy: Parse as a temporary struct that just has "stream" and "data" as RawValue?
                                // Or since we know the stream format: <symbol>@<type>
                                // We can peek at the JSON or try one then the other.
                                // Given simd-json mutability, we should clone if we fail?
                                // Actually, let's just try parsing as `CombinedStreamEvent<serde_json::Value>`? No, allocation.
                                
                                // Optimization: Check the "stream" field from the raw bytes?
                                // Or just try parsing `CombinedStreamEvent<BinanceBookTicker>` then `CombinedStreamEvent<BinanceTrade>`
                                
                                let mut bytes_copy = bytes.clone();
                                if let Ok(wrapper) = simd_json::from_slice::<CombinedStreamEvent<BinanceBookTicker>>(&mut bytes) {
                                    if let Ok(event) = self.convert_ticker(wrapper.data) {
                                        if let Err(e) = on_event(MarketMessageType::BookTicker as u16, &event) {
                                            error!("Failed to write to queue: {}", e);
                                        }
                                    }
                                } else if let Ok(wrapper) = simd_json::from_slice::<CombinedStreamEvent<BinanceTrade>>(&mut bytes_copy) {
                                    if let Ok(event) = self.convert_trade(wrapper.data) {
                                        if let Err(e) = on_event(MarketMessageType::Trade as u16, &event) {
                                            error!("Failed to write to queue: {}", e);
                                        }
                                    }
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
                }
                Err(e) => {
                    error!("Connection failed: {}. Retrying in 5s...", e);
                }
            }
            sleep(Duration::from_secs(5)).await;
        }
    }

    async fn connect_and_subscribe(&self) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
        // Construct combined stream URL
        // /stream?streams=<symbol>@bookTicker/<symbol>@trade
        let mut streams = Vec::new();
        for s in &self.symbols {
            streams.push(format!("{}@bookTicker", s.to_lowercase()));
            streams.push(format!("{}@trade", s.to_lowercase()));
        }
        
        let query = format!("streams={}", streams.join("/"));
        
        // Base URL is /ws, but for combined we need /stream
        let mut url = self.url.clone();
        url.set_path("stream");
        url.set_query(Some(&query));

        info!("Connecting to {}", url);
        let (ws_stream, _) = connect_async(url).await.context("Failed to connect")?;
        Ok(ws_stream)
    }

    fn convert_ticker(&self, ticker: BinanceBookTicker) -> Result<BookTicker> {
        // Parse strings to f64
        let bid_price = ticker.bid_price.parse::<f64>().unwrap_or_default();
        let bid_qty = ticker.bid_qty.parse::<f64>().unwrap_or_default();
        let ask_price = ticker.ask_price.parse::<f64>().unwrap_or_default();
        let ask_qty = ticker.ask_qty.parse::<f64>().unwrap_or_default();
        
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;

        Ok(BookTicker::new(
            now,
            bid_price,
            bid_qty,
            ask_price,
            ask_qty,
            &ticker.symbol,
        ))
    }

    fn convert_trade(&self, trade: BinanceTrade) -> Result<Trade> {
        let price = trade.price.parse::<f64>().unwrap_or_default();
        let qty = trade.qty.parse::<f64>().unwrap_or_default();

        Ok(Trade::new(
            trade.trade_time,
            price,
            qty,
            trade.trade_id,
            trade.buyer_order_id,
            trade.seller_order_id,
            trade.is_buyer_maker,
            &trade.symbol,
        ))
    }
}
