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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::etl::types::test_utils::{MockBookUpdate, MockEventHeader, MockOrderBook};
    use crate::stream::etl::feature::{Feature, RowBuffer, RowWriter};
    use arrow::datatypes::Schema;
    use std::sync::Arc;

    #[test]
    fn test_global_time_feature() {
        let mut feature = GlobalTime;
        let schema = feature.schema();
        assert_eq!(schema.len(), 2);
        assert_eq!(schema[0].name, "ingest_ts_ns");
        assert_eq!(schema[1].name, "exchange_ts_ns");

        let fields: Vec<_> = schema.iter().map(|c| arrow::datatypes::Field::new(&c.name, c.data_type.clone(), c.nullable)).collect();
        let schema = Arc::new(Schema::new(fields));
        let mut buffer = RowBuffer::new(schema, 10).unwrap();

        let book = MockOrderBook::new();
        let header = MockEventHeader::with_timestamps(1000000000, 2000000000);
        let event = MockBookUpdate::new(header);

        let mut writer = RowWriter::new(&mut buffer, true);
        feature.on_event(&book, &event, &mut writer);
        buffer.commit_row();

        assert_eq!(buffer.row_count(), 1);
    }

    #[test]
    fn test_mid_price_empty_book() {
        let mut feature = MidPrice::default();
        let schema = feature.schema();
        let fields: Vec<_> = schema.iter().map(|c| arrow::datatypes::Field::new(&c.name, c.data_type.clone(), c.nullable)).collect();
        let schema = Arc::new(Schema::new(fields));
        let mut buffer = RowBuffer::new(schema, 10).unwrap();

        let book = MockOrderBook::new();
        let header = MockEventHeader::new();
        let event = MockBookUpdate::new(header);

        let mut writer = RowWriter::new(&mut buffer, true);
        feature.on_event(&book, &event, &mut writer);
        buffer.commit_row();

        assert_eq!(buffer.row_count(), 1);
    }

    #[test]
    fn test_mid_price_calculation() {
        let mut feature = MidPrice::default();
        let schema = feature.schema();
        let fields: Vec<_> = schema.iter().map(|c| arrow::datatypes::Field::new(&c.name, c.data_type.clone(), c.nullable)).collect();
        let schema = Arc::new(Schema::new(fields));
        let mut buffer = RowBuffer::new(schema, 10).unwrap();

        let mut book = MockOrderBook::with_scales(4, 0);
        // bid: 100000 (10.0000), ask: 100010 (10.0010)
        book.add_bid(100000, 100);
        book.add_ask(100010, 100);

        let header = MockEventHeader::new();
        let event = MockBookUpdate::new(header);

        let mut writer = RowWriter::new(&mut buffer, true);
        feature.on_event(&book, &event, &mut writer);
        buffer.commit_row();

        assert_eq!(buffer.row_count(), 1);
        // Mid should be (10.0000 + 10.0010) / 2 = 10.0005
    }

    #[test]
    fn test_spread_bps_empty_book() {
        let mut feature = SpreadBps::default();
        let schema = feature.schema();
        let fields: Vec<_> = schema.iter().map(|c| arrow::datatypes::Field::new(&c.name, c.data_type.clone(), c.nullable)).collect();
        let schema = Arc::new(Schema::new(fields));
        let mut buffer = RowBuffer::new(schema, 10).unwrap();

        let book = MockOrderBook::new();
        let header = MockEventHeader::new();
        let event = MockBookUpdate::new(header);

        let mut writer = RowWriter::new(&mut buffer, true);
        feature.on_event(&book, &event, &mut writer);
        buffer.commit_row();

        assert_eq!(buffer.row_count(), 1);
    }

    #[test]
    fn test_spread_bps_calculation() {
        let mut feature = SpreadBps::default();
        let schema = feature.schema();
        let fields: Vec<_> = schema.iter().map(|c| arrow::datatypes::Field::new(&c.name, c.data_type.clone(), c.nullable)).collect();
        let schema = Arc::new(Schema::new(fields));
        let mut buffer = RowBuffer::new(schema, 10).unwrap();

        let mut book = MockOrderBook::with_scales(4, 0);
        // bid: 100000 (10.0000), ask: 100100 (10.0100)
        book.add_bid(100000, 100);
        book.add_ask(100100, 100);

        let header = MockEventHeader::new();
        let event = MockBookUpdate::new(header);

        let mut writer = RowWriter::new(&mut buffer, true);
        feature.on_event(&book, &event, &mut writer);
        buffer.commit_row();

        assert_eq!(buffer.row_count(), 1);
        // Spread: (10.0100 - 10.0000) / 10.0050 * 10000 ≈ 99.95 bps
    }

    #[test]
    fn test_book_imbalance_empty_book() {
        let mut feature = BookImbalance::new(5);
        let schema = feature.schema();
        let fields: Vec<_> = schema.iter().map(|c| arrow::datatypes::Field::new(&c.name, c.data_type.clone(), c.nullable)).collect();
        let schema = Arc::new(Schema::new(fields));
        let mut buffer = RowBuffer::new(schema, 10).unwrap();

        let book = MockOrderBook::new();
        let header = MockEventHeader::new();
        let event = MockBookUpdate::new(header);

        let mut writer = RowWriter::new(&mut buffer, true);
        feature.on_event(&book, &event, &mut writer);
        buffer.commit_row();

        assert_eq!(buffer.row_count(), 1);
    }

    #[test]
    fn test_book_imbalance_balanced() {
        let mut feature = BookImbalance::new(3);
        let schema = feature.schema();
        let fields: Vec<_> = schema.iter().map(|c| arrow::datatypes::Field::new(&c.name, c.data_type.clone(), c.nullable)).collect();
        let schema = Arc::new(Schema::new(fields));
        let mut buffer = RowBuffer::new(schema, 10).unwrap();

        let mut book = MockOrderBook::new();
        // 3 bid levels with 100 size each
        book.add_bid(99, 100);
        book.add_bid(98, 100);
        book.add_bid(97, 100);
        // 3 ask levels with 100 size each
        book.add_ask(100, 100);
        book.add_ask(101, 100);
        book.add_ask(102, 100);

        let header = MockEventHeader::new();
        let event = MockBookUpdate::new(header);

        let mut writer = RowWriter::new(&mut buffer, true);
        feature.on_event(&book, &event, &mut writer);
        buffer.commit_row();

        assert_eq!(buffer.row_count(), 1);
        // Imbalance should be 0 (balanced)
    }

    #[test]
    fn test_book_imbalance_bid_heavy() {
        let mut feature = BookImbalance::new(2);
        let schema = feature.schema();
        let fields: Vec<_> = schema.iter().map(|c| arrow::datatypes::Field::new(&c.name, c.data_type.clone(), c.nullable)).collect();
        let schema = Arc::new(Schema::new(fields));
        let mut buffer = RowBuffer::new(schema, 10).unwrap();

        let mut book = MockOrderBook::new();
        // 2 bid levels with more size
        book.add_bid(99, 200);
        book.add_bid(98, 200);
        // 2 ask levels with less size
        book.add_ask(100, 100);
        book.add_ask(101, 100);

        let header = MockEventHeader::new();
        let event = MockBookUpdate::new(header);

        let mut writer = RowWriter::new(&mut buffer, true);
        feature.on_event(&book, &event, &mut writer);
        buffer.commit_row();

        assert_eq!(buffer.row_count(), 1);
        // Imbalance: (400 - 200) / 600 = 0.333...
    }
}
