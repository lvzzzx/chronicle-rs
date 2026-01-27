use arrow::datatypes::DataType;

use crate::stream::replay::{BookEvent, L2Book};

use super::feature::{ColumnSpec, Feature, RowWriter};

#[derive(Debug, Default, Clone, Copy)]
pub struct GlobalTime;

impl Feature for GlobalTime {
    fn schema(&self) -> Vec<ColumnSpec> {
        vec![
            ColumnSpec::new("ingest_ts_ns", DataType::UInt64),
            ColumnSpec::new("exchange_ts_ns", DataType::UInt64),
        ]
    }

    fn on_event(&mut self, _book: &L2Book, event: &BookEvent<'_>, out: &mut RowWriter<'_>) {
        out.append_u64(0, event.header.ingest_ts_ns);
        out.append_u64(1, event.header.exchange_ts_ns);
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct MidPrice {
    last_scale: u8,
    price_factor: f64,
}

impl MidPrice {
    fn price_factor(&mut self, book: &L2Book) -> f64 {
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

    fn on_event(&mut self, book: &L2Book, _event: &BookEvent<'_>, out: &mut RowWriter<'_>) {
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
    fn price_factor(&mut self, book: &L2Book) -> f64 {
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

    fn on_event(&mut self, book: &L2Book, _event: &BookEvent<'_>, out: &mut RowWriter<'_>) {
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

    fn on_event(&mut self, book: &L2Book, _event: &BookEvent<'_>, out: &mut RowWriter<'_>) {
        let bid_sum = sum_depth(book.bids().iter().rev(), self.depth);
        let ask_sum = sum_depth(book.asks().iter(), self.depth);
        let denom = bid_sum + ask_sum;
        if denom == 0 {
            out.append_f64(0, 0.0);
            return;
        }
        let imbalance = (bid_sum as f64 - ask_sum as f64) / denom as f64;
        out.append_f64(0, imbalance);
    }
}

fn best_prices(book: &L2Book) -> Option<(u64, u64)> {
    let bid = book.bids().iter().next_back().map(|(p, _)| *p)?;
    let ask = book.asks().iter().next().map(|(p, _)| *p)?;
    Some((bid, ask))
}

fn sum_depth<'a, I>(iter: I, depth: usize) -> u64
where
    I: Iterator<Item = (&'a u64, &'a u64)>,
{
    iter.take(depth).map(|(_, size)| *size).sum()
}
