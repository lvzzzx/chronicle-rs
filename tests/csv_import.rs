use std::fs::File;
use std::io::Write;

use flate2::write::GzEncoder;
use flate2::Compression;
use tempfile::tempdir;

use chronicle::core::segment::{DEFAULT_SEGMENT_SIZE, SEG_FLAG_SEALED};
use chronicle::import::tardis_csv::import_tardis_incremental_book;
use chronicle::layout::ArchiveLayout;
use chronicle::storage::read_segment_flags;

#[test]
fn csv_import_writes_sealed_segment() {
    let dir = tempdir().expect("tempdir");
    let input_path = dir.path().join("sample.csv.gz");

    let csv_data = concat!(
        "exchange,symbol,timestamp,local_timestamp,is_snapshot,side,price,amount\n",
        "binance-futures,BTCUSDT,1000000,1000001,true,bid,100.0,1.0\n",
        "binance-futures,BTCUSDT,1000000,1000001,true,ask,101.0,2.0\n",
        "binance-futures,BTCUSDT,1000100,1000100,false,bid,100.0,0\n",
        "binance-futures,BTCUSDT,1000100,1000100,false,ask,101.0,1.5\n",
    );

    let file = File::create(&input_path).expect("create gz");
    let mut encoder = GzEncoder::new(file, Compression::default());
    encoder.write_all(csv_data.as_bytes()).expect("write csv");
    encoder.finish().expect("finish gz");

    let archive_root = dir.path().join("archive");

    let stats = import_tardis_incremental_book(
        &input_path,
        &archive_root,
        "binance-futures",
        "BTCUSDT",
        "2024-05-01",
        "book",
        1,
        42,
        DEFAULT_SEGMENT_SIZE,
        None,
        None,
        false,
        false,
    )
    .expect("import csv");

    assert!(stats.rows > 0);
    assert!(stats.groups > 0);

    let layout = ArchiveLayout::new(&archive_root);
    let stream_dir = layout
        .stream_dir("binance-futures", "BTCUSDT", "2024-05-01", "book")
        .expect("stream dir");

    let mut found = None;
    for entry in std::fs::read_dir(&stream_dir).expect("read stream dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("q") {
            found = Some(path);
            break;
        }
    }

    let segment = found.expect("segment file");
    let flags = read_segment_flags(&segment).expect("segment flags");
    assert_ne!(flags & SEG_FLAG_SEALED, 0);
}
