use arrow::datatypes::DataType;

use super::feature::{ColumnSpec, Feature, RowWriter};
use super::types::{BookUpdate, OrderBook};

#[derive(Debug, Default, Clone, Copy)]
pub struct GlobalTime;

impl Feature for GlobalTime {
    fn schema(&self) -> Vec<ColumnSpec> {
        vec![
            ColumnSpec::new("ingest_ts_ns", DataType::UInt64),
            ColumnSpec::new("exchange_ts_ns", DataType::UInt64),
        ]
    }

    fn on_event(&mut self, _book: &dyn OrderBook, event: &dyn BookUpdate, out: &mut RowWriter<'_>) {
        let header = event.header();
        out.append_u64(0, header.ingest_ts_ns());
        out.append_u64(1, header.exchange_ts_ns());
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct MidPrice {
    last_scale: u8,
    price_factor: f64,
}

impl MidPrice {
    fn price_factor(&mut self, book: &dyn OrderBook) -> f64 {
        let (price_scale, _) = book.scales();
        if price_scale != self.last_scale || self.price_factor == 0.0 {
            self.last_scale = price_scale;
            self.price_factor = 10f64.powi(price_scale as i32);
        }
        self.price_factor
    }
}

impl Feature for MidPrice {
    fn schema(&self) -> Vec<ColumnSpec> {
        vec![ColumnSpec::new("mid_price", DataType::Float64)]
    }

    fn on_event(&mut self, book: &dyn OrderBook, _event: &dyn BookUpdate, out: &mut RowWriter<'_>) {
        let Some((bid, ask)) = best_prices(book) else {
            out.append_f64(0, 0.0);
            return;
        };
        let factor = self.price_factor(book);
        let bid = bid as f64 / factor;
        let ask = ask as f64 / factor;
        out.append_f64(0, (bid + ask) * 0.5);
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SpreadBps {
    last_scale: u8,
    price_factor: f64,
}

impl SpreadBps {
    fn price_factor(&mut self, book: &dyn OrderBook) -> f64 {
        let (price_scale, _) = book.scales();
        if price_scale != self.last_scale || self.price_factor == 0.0 {
            self.last_scale = price_scale;
            self.price_factor = 10f64.powi(price_scale as i32);
        }
        self.price_factor
    }
}

impl Feature for SpreadBps {
    fn schema(&self) -> Vec<ColumnSpec> {
        vec![ColumnSpec::new("spread_bps", DataType::Float64)]
    }

    fn on_event(&mut self, book: &dyn OrderBook, _event: &dyn BookUpdate, out: &mut RowWriter<'_>) {
        let Some((bid, ask)) = best_prices(book) else {
            out.append_f64(0, 0.0);
            return;
        };
        let factor = self.price_factor(book);
        let bid = bid as f64 / factor;
        let ask = ask as f64 / factor;
        let mid = (bid + ask) * 0.5;
        if mid == 0.0 {
            out.append_f64(0, 0.0);
            return;
        }
        let spread = (ask - bid) / mid * 10_000.0;
        out.append_f64(0, spread);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BookImbalance {
    depth: usize,
}

impl BookImbalance {
    pub fn new(depth: usize) -> Self {
        Self {
            depth: depth.max(1),
        }
    }
}

impl Feature for BookImbalance {
    fn schema(&self) -> Vec<ColumnSpec> {
        vec![ColumnSpec::new("book_imbalance", DataType::Float64)]
    }

    fn on_event(&mut self, book: &dyn OrderBook, _event: &dyn BookUpdate, out: &mut RowWriter<'_>) {
        let bid_sum = sum_depth(book.bid_levels(), self.depth);
        let ask_sum = sum_depth(book.ask_levels(), self.depth);
        let denom = bid_sum + ask_sum;
        if denom == 0 {
            out.append_f64(0, 0.0);
            return;
        }
        let imbalance = (bid_sum as f64 - ask_sum as f64) / denom as f64;
        out.append_f64(0, imbalance);
    }
}

fn best_prices(book: &dyn OrderBook) -> Option<(u64, u64)> {
    let bid = book.best_bid()?.0;
    let ask = book.best_ask()?.0;
    Some((bid, ask))
}

fn sum_depth(iter: Box<dyn Iterator<Item = (u64, u64)> + '_>, depth: usize) -> u64 {
    iter.take(depth).map(|(_, size)| size).sum()
}
