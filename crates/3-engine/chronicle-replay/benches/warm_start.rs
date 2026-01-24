use chronicle_core::Queue;
use chronicle_protocol::{
    book_flags, BookEventHeader, BookEventType, BookMode, L2Diff, L2Snapshot, PriceLevelUpdate,
    TypeId, PROTOCOL_VERSION,
};
use chronicle_replay::snapshot::{
    SnapshotMetadata, SnapshotRetention, SnapshotWriter, SNAPSHOT_VERSION,
};
use chronicle_replay::ReplayEngine;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::mem::size_of;

const LEVEL_COUNT: usize = 1024;
const DIFF_EVENTS: usize = 10_000;

fn push_struct<T: Copy>(buf: &mut Vec<u8>, value: &T) {
    let bytes = unsafe {
        std::slice::from_raw_parts((value as *const T) as *const u8, size_of::<T>())
    };
    buf.extend_from_slice(bytes);
}

fn build_l2_snapshot_payload(
    price_scale: u8,
    size_scale: u8,
    bids: &[PriceLevelUpdate],
    asks: &[PriceLevelUpdate],
) -> Vec<u8> {
    let snapshot = L2Snapshot {
        price_scale,
        size_scale,
        _pad0: 0,
        bid_count: bids.len() as u32,
        ask_count: asks.len() as u32,
    };
    let mut buf = Vec::with_capacity(
        size_of::<L2Snapshot>() + (bids.len() + asks.len()) * size_of::<PriceLevelUpdate>(),
    );
    push_struct(&mut buf, &snapshot);
    for level in bids {
        push_struct(&mut buf, level);
    }
    for level in asks {
        push_struct(&mut buf, level);
    }
    buf
}

fn build_book_event_payload(
    seq: u64,
    event_type: BookEventType,
    ingest_ts_ns: u64,
    exchange_ts_ns: u64,
    body: &[u8],
) -> Vec<u8> {
    let record_len = (size_of::<BookEventHeader>() + body.len()) as u16;
    let header = BookEventHeader {
        schema_version: PROTOCOL_VERSION,
        record_len,
        endianness: 0,
        _pad0: 0,
        venue_id: 7,
        market_id: 11,
        stream_id: 0,
        ingest_ts_ns,
        exchange_ts_ns,
        seq,
        native_seq: 0,
        event_type: event_type as u8,
        book_mode: BookMode::L2 as u8,
        flags: 0,
        _pad1: 0,
    };
    let mut buf = Vec::with_capacity(size_of::<BookEventHeader>() + body.len());
    push_struct(&mut buf, &header);
    buf.extend_from_slice(body);
    buf
}

fn prepare_queue(tmp: &tempfile::TempDir) -> anyhow::Result<std::path::PathBuf> {
    let mut writer = Queue::open_publisher(tmp.path())?;

    let mut bids = Vec::with_capacity(LEVEL_COUNT);
    let mut asks = Vec::with_capacity(LEVEL_COUNT);
    for i in 0..LEVEL_COUNT {
        bids.push(PriceLevelUpdate {
            price: 1000 + i as u64,
            size: 1,
        });
        asks.push(PriceLevelUpdate {
            price: 2000 + i as u64,
            size: 1,
        });
    }

    let snap_body = build_l2_snapshot_payload(2, 3, &bids, &asks);
    let snap_payload = build_book_event_payload(0, BookEventType::Snapshot, 1_000, 1_000, &snap_body);
    writer.append_with_timestamp(TypeId::BookEvent.as_u16(), &snap_payload, 1_000)?;

    for i in 0..DIFF_EVENTS {
        let diff = L2Diff {
            update_id_first: 0,
            update_id_last: 0,
            update_id_prev: 0,
            price_scale: 2,
            size_scale: 3,
            flags: book_flags::ABSOLUTE,
            bid_count: 1,
            ask_count: 0,
        };
        let mut diff_body =
            Vec::with_capacity(size_of::<L2Diff>() + size_of::<PriceLevelUpdate>());
        push_struct(&mut diff_body, &diff);
        push_struct(
            &mut diff_body,
            &PriceLevelUpdate {
                price: 1000 + (i % LEVEL_COUNT) as u64,
                size: (i as u64) + 2,
            },
        );
        let ts = 2_000 + i as u64;
        let payload = build_book_event_payload(
            (i + 1) as u64,
            BookEventType::Diff,
            ts,
            ts,
            &diff_body,
        );
        writer.append_with_timestamp(TypeId::BookEvent.as_u16(), &payload, ts)?;
    }

    let snapshot_payload = build_l2_snapshot_payload(2, 3, &bids, &asks);
    let snapshot_writer = SnapshotWriter::new(tmp.path(), SnapshotRetention::default());
    let metadata = SnapshotMetadata {
        schema_version: SNAPSHOT_VERSION,
        endianness: 0,
        book_mode: BookMode::L2 as u16,
        venue_id: 7,
        market_id: 11,
        seq_num: (DIFF_EVENTS / 2) as u64,
        ingest_ts_ns_start: 1_000,
        ingest_ts_ns_end: 2_000 + (DIFF_EVENTS as u64 / 2),
        exchange_ts_ns_start: 1_000,
        exchange_ts_ns_end: 2_000 + (DIFF_EVENTS as u64 / 2),
        book_hash: [0u8; 16],
        flags: 0,
    };
    snapshot_writer.write_snapshot(&metadata, &snapshot_payload)?;

    Ok(tmp.path().to_path_buf())
}

fn bench_warm_start(c: &mut Criterion) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = prepare_queue(&tmp).expect("prepare queue");
    let target_ts = 2_000 + (DIFF_EVENTS as u64 / 2) + 1_000;

    let mut group = c.benchmark_group("warm_start");
    group.bench_with_input(
        BenchmarkId::new("snapshot_to_exchange_ts", DIFF_EVENTS),
        &root,
        |b, root| {
            b.iter(|| {
                let mut engine = ReplayEngine::open(root, "bench")?;
                engine.warm_start_to_exchange_ts(7, 11, target_ts)?;
                anyhow::Ok(())
            })
        },
    );

    group.bench_with_input(
        BenchmarkId::new("cold_replay_to_exchange_ts", DIFF_EVENTS),
        &root,
        |b, root| {
            b.iter(|| {
                let mut engine = ReplayEngine::open(root, "bench_cold")?;
                engine.reader_mut().seek_seq(0)?;
                engine.warm_start_to_exchange_ts(7, 11, target_ts)?;
                anyhow::Ok(())
            })
        },
    );

    group.finish();
}

criterion_group!(benches, bench_warm_start);
criterion_main!(benches);
