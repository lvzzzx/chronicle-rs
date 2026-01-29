use chronicle::stream::etl::SymbolCatalog;

#[test]
fn resolves_symbol_versions_by_event_time() {
    let path = std::path::PathBuf::from("tests/data/dim_symbol");
    let catalog = SymbolCatalog::load_delta(&path).expect("load catalog");

    let market_id = SymbolCatalog::market_id_for_symbol("BTCUSDT");

    let first = catalog
        .resolve_by_market_id(1, market_id, 1_500_000)
        .expect("resolve first");
    assert_eq!(first.symbol_id, 101);

    let boundary = catalog
        .resolve_by_market_id(1, market_id, 2_000_000)
        .expect("resolve boundary");
    assert_eq!(boundary.symbol_id, 202);

    let later = catalog
        .resolve_by_market_id(1, market_id, 9_000_000)
        .expect("resolve later");
    assert_eq!(later.symbol_id, 202);
}
