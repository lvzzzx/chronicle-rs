use chronicle::core::Queue;
use chronicle::protocol::{book_flags, BookEventHeader, BookEventType, BookMode, L2Diff, L2Snapshot, PriceLevelUpdate, TypeId, PROTOCOL_VERSION};
use chronicle::replay::snapshot::{SnapshotMetadata, SnapshotRetention, SnapshotWriter, SNAPSHOT_VERSION};
use chronicle::replay::ReplayEngine;
use std::mem::size_of;

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

#[test]
fn warm_start_from_snapshot_replays_forward() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let mut writer = Queue::open_publisher(dir.path())?;

    let bid = PriceLevelUpdate { price: 100, size: 1 };
    let bid_updated = PriceLevelUpdate { price: 100, size: 2 };
    let ask = PriceLevelUpdate { price: 101, size: 3 };

    // seq0 snapshot event
    let snap_body = build_l2_snapshot_payload(2, 3, &[bid], &[]);
    let snap_payload = build_book_event_payload(0, BookEventType::Snapshot, 1_000, 1_000, &snap_body);
    writer.append_with_timestamp(TypeId::BookEvent.as_u16(), &snap_payload, 1_000)?;

    // seq1 diff event (update bid size)
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
    let mut diff_body = Vec::with_capacity(size_of::<L2Diff>() + size_of::<PriceLevelUpdate>());
    push_struct(&mut diff_body, &diff);
    push_struct(&mut diff_body, &bid_updated);
    let diff_payload = build_book_event_payload(1, BookEventType::Diff, 2_000, 2_000, &diff_body);
    writer.append_with_timestamp(TypeId::BookEvent.as_u16(), &diff_payload, 2_000)?;

    // seq2 diff event (add ask)
    let diff2 = L2Diff {
        update_id_first: 0,
        update_id_last: 0,
        update_id_prev: 0,
        price_scale: 2,
        size_scale: 3,
        flags: book_flags::ABSOLUTE,
        bid_count: 0,
        ask_count: 1,
    };
    let mut diff2_body = Vec::with_capacity(size_of::<L2Diff>() + size_of::<PriceLevelUpdate>());
    push_struct(&mut diff2_body, &diff2);
    push_struct(&mut diff2_body, &ask);
    let diff2_payload = build_book_event_payload(2, BookEventType::Diff, 3_000, 3_000, &diff2_body);
    writer.append_with_timestamp(TypeId::BookEvent.as_u16(), &diff2_payload, 3_000)?;

    // Snapshot file representing state after seq1.
    let snapshot_payload = build_l2_snapshot_payload(2, 3, &[bid_updated], &[]);
    let snapshot_writer = SnapshotWriter::new(dir.path(), SnapshotRetention::default());
    let metadata = SnapshotMetadata {
        schema_version: SNAPSHOT_VERSION,
        endianness: 0,
        book_mode: BookMode::L2 as u16,
        venue_id: 7,
        market_id: 11,
        seq_num: 1,
        ingest_ts_ns_start: 1_000,
        ingest_ts_ns_end: 2_000,
        exchange_ts_ns_start: 1_000,
        exchange_ts_ns_end: 2_000,
        book_hash: [0u8; 16],
        flags: 0,
    };
    let snap_path = snapshot_writer.write_snapshot(&metadata, &snapshot_payload)?;

    let mut engine = ReplayEngine::open(dir.path(), "warm_start")?;
    let header = engine.warm_start_from_snapshot_path(&snap_path)?;
    assert_eq!(header.seq_num, 1);

    let update = engine.next()?.expect("expected diff event");
    match update {
        chronicle::replay::ReplayUpdate::Applied { seq, event_type } => {
            assert_eq!(seq, 2);
            assert_eq!(event_type, BookEventType::Diff);
        }
        other => panic!("unexpected update: {:?}", other),
    }

    let book = engine.l2_book();
    assert_eq!(book.bids().get(&100).copied(), Some(2));
    assert_eq!(book.asks().get(&101).copied(), Some(3));
    Ok(())
}
