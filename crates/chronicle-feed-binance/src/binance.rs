use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use log::{error, info, warn};
use serde::Deserialize;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use url::Url;

use crate::market::{BookTicker, MarketMessageType};

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
    // We can use local timestamp if not provided, or 'E' if available (Event time)
    // bookTicker payload doesn't always have 'E' in the raw stream, but combined stream does?
    // Let's check docs. "The Individual Symbol Book Ticker Streams" payload:
    // { u, s, b, B, a, A } - No timestamp.
    // We will use local receipt time for now as 'exchange timestamp' is missing in this specific payload
    // unless we use @depth or @trade.
}

// For combined streams, the payload is wrapped in {"stream": "...", "data": ...}
#[derive(Deserialize, Debug)]
struct CombinedStreamEvent<T> {
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
        F: FnMut(u16, &[u8]) -> Result<()>,
    {
        loop {
            info!("Connecting to Binance WS...");
            match self.connect_and_subscribe().await {
                Ok(mut ws_stream) => {
                    info!("Connected. Processing messages...");
                    while let Some(msg) = ws_stream.next().await {
                        match msg {
                            Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                                // Try parsing as BookTicker
                                // Note: We are using raw stream or combined? 
                                // Ideally combined stream for multiple symbols: stream.binance.com:9443/stream?streams=<symbol>@bookTicker
                                
                                if let Ok(ticker) = serde_json::from_str::<BinanceBookTicker>(&text) {
                                    if let Ok(event) = self.convert_ticker(ticker) {
                                        // unsafe cast to bytes for now (repr(C))
                                        let bytes = unsafe {
                                            std::slice::from_raw_parts(
                                                &event as *const BookTicker as *const u8,
                                                std::mem::size_of::<BookTicker>(),
                                            )
                                        };
                                        if let Err(e) = on_event(MarketMessageType::BookTicker as u16, bytes) {
                                            error!("Failed to write to queue: {}", e);
                                        }
                                    }
                                } else if let Ok(wrapper) = serde_json::from_str::<CombinedStreamEvent<BinanceBookTicker>>(&text) {
                                     if let Ok(event) = self.convert_ticker(wrapper.data) {
                                        let bytes = unsafe {
                                            std::slice::from_raw_parts(
                                                &event as *const BookTicker as *const u8,
                                                std::mem::size_of::<BookTicker>(),
                                            )
                                        };
                                        if let Err(e) = on_event(MarketMessageType::BookTicker as u16, bytes) {
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
        // /stream?streams=<symbol>@bookTicker/<symbol>@bookTicker
        let streams: Vec<String> = self.symbols.iter()
            .map(|s| format!("{}@bookTicker", s.to_lowercase()))
            .collect();
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
}
