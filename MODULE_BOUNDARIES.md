# Chronicle-RS Module Boundaries

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              APPLICATIONS                                   │
│  • chronicle_szse_recon (SZSE L3 reconstruction)                           │
│  • chronicle_etl (Feature extraction)                                      │
│  • User applications                                                       │
└──────────┬──────────────────────┬────────────────────────┬─────────────────┘
           │                      │                        │
           │                      │                        │
┌──────────▼──────────┐  ┌────────▼──────────┐  ┌─────────▼──────────┐
│   VENUES MODULE     │  │  STREAM MODULE    │  │  STORAGE MODULE    │
│  (Venue-Specific)   │  │  (Generic)        │  │  (Persistence)     │
└──────────┬──────────┘  └────────┬──────────┘  └────────────────────┘
           │                      │
           │                      │
┌──────────▼──────────────────────▼──────────────────────────────────────┐
│                       STREAM SUBSYSTEM                                 │
│                                                                        │
│  ┌─────────────────────────────────────────────────────────────────┐  │
│  │ stream/sequencer/  [SHARED UTILITY]                             │  │
│  │ ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━  │  │
│  │ • SequenceValidator: Monotonic sequence validation              │  │
│  │ • GapPolicy: Panic | Quarantine | Ignore                        │  │
│  │ • GapDetection: Sequential | Gap { from, to }                   │  │
│  │                                                                  │  │
│  │ Used by: replay/, venues/szse/l3/                               │  │
│  └─────────────────────────────────────────────────────────────────┘  │
│                                    │                                   │
│                                    │                                   │
│  ┌─────────────────────────────────▼───────────────────────────────┐  │
│  │ stream/replay/  [CORE ENGINE]                                   │  │
│  │ ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━  │  │
│  │ • ReplayEngine<R: StreamReader>                                 │  │
│  │   - Transform queue messages → book state                       │  │
│  │   - Uses SequenceValidator internally                           │  │
│  │ • L2Book: BTreeMap-based orderbook                              │  │
│  │ • ReplayMessage: Replay output with book + event                │  │
│  │                                                                  │  │
│  │ Exports: BookEvent, BookEventPayload, L2Book, LivReplayEngine  │  │
│  └─────────────────────────────────────────────────────────────────┘  │
│                                    │                                   │
│                                    │                                   │
│  ┌─────────────────────────────────▼───────────────────────────────┐  │
│  │ stream/etl/  [FEATURE EXTRACTION]                               │  │
│  │ ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━  │  │
│  │                                                                  │  │
│  │  ┌──────────────────────────────────────────────────────────┐  │  │
│  │  │ etl/types.rs  [ABSTRACTION LAYER]                        │  │  │
│  │  │ ───────────────────────────────────────────────────────  │  │  │
│  │  │ Traits (decoupling from replay):                         │  │  │
│  │  │   • OrderBook                                            │  │  │
│  │  │   • EventHeader                                          │  │  │
│  │  │   • BookUpdate                                           │  │  │
│  │  │   • Message                                              │  │  │
│  │  │   • MessageSource                                        │  │  │
│  │  │                                                           │  │  │
│  │  │ Adapters:                                                │  │  │
│  │  │   impl OrderBook for L2Book                             │  │  │
│  │  │   impl EventHeader for BookEventHeader                  │  │  │
│  │  │   impl BookUpdate for BookEvent                         │  │  │
│  │  │   impl Message for ReplayMessage                        │  │  │
│  │  │   impl MessageSource for ReplayEngine                   │  │  │
│  │  │                                                           │  │  │
│  │  │ Test Utilities:                                          │  │  │
│  │  │   • MockOrderBook                                        │  │  │
│  │  │   • MockEventHeader                                      │  │  │
│  │  │   • MockBookUpdate                                       │  │  │
│  │  └──────────────────────────────────────────────────────────┘  │  │
│  │                         │                                       │  │
│  │  ┌──────────────────────▼──────────────────────────────────┐  │  │
│  │  │ etl/feature.rs  [FRAMEWORK]                              │  │  │
│  │  │ ───────────────────────────────────────────────────────  │  │  │
│  │  │ • Feature trait: schema() + on_event()                   │  │  │
│  │  │ • FeatureSet trait: calculate() + should_emit()          │  │  │
│  │  │ • Trigger trait: should_emit()                           │  │  │
│  │  │ • FeatureGraph: Composable multi-feature                 │  │  │
│  │  │ • RowBuffer: Arrow columnar buffer                       │  │  │
│  │  │ • RowWriter: Conditional column writer                   │  │  │
│  │  └──────────────────────────────────────────────────────────┘  │  │
│  │                         │                                       │  │
│  │  ┌──────────────────────▼──────────────────────────────────┐  │  │
│  │  │ etl/features.rs  [IMPLEMENTATIONS]                       │  │  │
│  │  │ ───────────────────────────────────────────────────────  │  │  │
│  │  │ Concrete features (all tested in isolation):             │  │  │
│  │  │   • GlobalTime: Extract timestamps                       │  │  │
│  │  │   • MidPrice: Calculate mid-price                        │  │  │
│  │  │   • SpreadBps: Calculate spread in bps                   │  │  │
│  │  │   • BookImbalance: Calculate imbalance ratio             │  │  │
│  │  └──────────────────────────────────────────────────────────┘  │  │
│  │                         │                                       │  │
│  │  ┌──────────────────────▼──────────────────────────────────┐  │  │
│  │  │ etl/triggers.rs  [EVENT FILTERING]                       │  │  │
│  │  │ ───────────────────────────────────────────────────────  │  │  │
│  │  │ • TimeBarTrigger: Time-bucket emission                   │  │  │
│  │  │ • EventTypeTrigger: Filter by event type                 │  │  │
│  │  │ • AnyTrigger: Logical OR composition                     │  │  │
│  │  └──────────────────────────────────────────────────────────┘  │  │
│  │                                                                  │  │
│  │  ┌──────────────────────────────────────────────────────────┐  │  │
│  │  │ etl/catalog.rs  [SYMBOL RESOLUTION]                      │  │  │
│  │  │ ───────────────────────────────────────────────────────  │  │  │
│  │  │ • SymbolCatalog: Time-aware symbol lookup                │  │  │
│  │  │ • SymbolIdentity: Symbol metadata                        │  │  │
│  │  └──────────────────────────────────────────────────────────┘  │  │
│  │                                                                  │  │
│  │  ┌──────────────────────────────────────────────────────────┐  │  │
│  │  │ etl/extractor.rs  [ORCHESTRATION]                        │  │  │
│  │  │ ───────────────────────────────────────────────────────  │  │  │
│  │  │ • Extractor<F, S>: Feature → Sink pipeline               │  │  │
│  │  └──────────────────────────────────────────────────────────┘  │  │
│  │                                                                  │  │
│  │  ┌──────────────────────────────────────────────────────────┐  │  │
│  │  │ etl/refinery.rs  [STREAM TRANSFORMATION]                 │  │  │
│  │  │ ───────────────────────────────────────────────────────  │  │  │
│  │  │ • Refinery: Inject synthetic snapshots                   │  │  │
│  │  │ • Uses protocol::serialization                           │  │  │
│  │  └──────────────────────────────────────────────────────────┘  │  │
│  │                                                                  │  │
│  │  ┌──────────────────────────────────────────────────────────┐  │  │
│  │  │ etl/sink.rs  [OUTPUT ABSTRACTION]                        │  │  │
│  │  │ ───────────────────────────────────────────────────────  │  │  │
│  │  │ • RowSink trait: write_batch()                           │  │  │
│  │  │ • ParquetSink<W>: Write to Parquet                       │  │  │
│  │  └──────────────────────────────────────────────────────────┘  │  │
│  └──────────────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────────┐
│                          VENUES SUBSYSTEM                                   │
│                                                                             │
│  ┌───────────────────────────────────────────────────────────────────────┐ │
│  │ venues/szse/l3/  [SZSE L3 RECONSTRUCTION]                             │ │
│  │ ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ │ │
│  │ • SzseL3Engine: Per-channel L3 orderbook reconstruction              │ │
│  │   - Uses SequenceValidator for gap handling                          │ │
│  │   - Multi-worker parallel processing                                 │ │
│  │ • SzseL3Dispatcher: Route orders to workers by hash                  │ │
│  │ • SzseL3Worker: Per-symbol L3 book builders                          │ │
│  │ • L3Book: Individual order tracking                                  │ │
│  │                                                                       │ │
│  │ Re-exports: GapPolicy from stream::sequencer                         │ │
│  └───────────────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────────┐
│                          PROTOCOL MODULE                                    │
│                                                                             │
│  ┌───────────────────────────────────────────────────────────────────────┐ │
│  │ protocol/mod.rs  [WIRE PROTOCOL]                                      │ │
│  │ ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ │ │
│  │ Types:                                                                │ │
│  │   • BookEventHeader (#[repr(C)])                                     │ │
│  │   • L2Snapshot, L2Diff (#[repr(C)])                                  │ │
│  │   • PriceLevelUpdate (#[repr(C)])                                    │ │
│  │   • L3Event (#[repr(C)])                                             │ │
│  │   • BookEventType, BookMode, L3EventType (enums)                     │ │
│  │                                                                       │ │
│  │ Serialization Module:                                                │ │
│  │   • write_l2_snapshot(): Serialize L2 snapshot to bytes              │ │
│  │   • l2_snapshot_size(): Calculate buffer size                        │ │
│  │   • struct_to_bytes(): Internal unsafe helper                        │ │
│  │                                                                       │ │
│  │ Used by: All modules that read/write protocol messages               │ │
│  └───────────────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────────┐
│                          TRADING MODULE                                     │
│                                                                             │
│  ┌───────────────────────────────────────────────────────────────────────┐ │
│  │ trading/  [DOMAIN LAYER]                                              │ │
│  │ ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ │ │
│  │                                                                       │ │
│  │  ┌─────────────────────────────────────────────────────────────────┐ │ │
│  │  │ trading/paths.rs  [PATH CONVENTIONS]                            │ │ │
│  │  │ ───────────────────────────────────────────────────────────────  │ │ │
│  │  │ Functions:                                                       │ │ │
│  │  │   • strategy_orders_out_path(root, strategy_id)                 │ │ │
│  │  │   • strategy_orders_in_path(root, strategy_id)                  │ │ │
│  │  │   • validate_component() - Path traversal prevention            │ │ │
│  │  └─────────────────────────────────────────────────────────────────┘ │ │
│  │                                                                       │ │
│  │  ┌─────────────────────────────────────────────────────────────────┐ │ │
│  │  │ trading/strategy.rs  [STRATEGY-SIDE CHANNEL]                    │ │ │
│  │  │ ───────────────────────────────────────────────────────────────  │ │ │
│  │  │ • StrategyChannel: Domain wrapper over BidirectionalChannel     │ │ │
│  │  │   - send_order(order: &[u8])                                    │ │ │
│  │  │   - recv_fill() -> MessageView                                  │ │ │
│  │  │   - commit_fill()                                               │ │ │
│  │  └─────────────────────────────────────────────────────────────────┘ │ │
│  │                                                                       │ │
│  │  ┌─────────────────────────────────────────────────────────────────┐ │ │
│  │  │ trading/router.rs  [ROUTER-SIDE CHANNEL]                        │ │ │
│  │  │ ───────────────────────────────────────────────────────────────  │ │ │
│  │  │ • RouterChannel: Domain wrapper over BidirectionalChannel       │ │ │
│  │  │   - recv_order() -> MessageView                                 │ │ │
│  │  │   - send_fill(fill: &[u8])                                      │ │ │
│  │  │   - commit_order()                                              │ │ │
│  │  │   - try_connect() - Non-blocking discovery                      │ │ │
│  │  └─────────────────────────────────────────────────────────────────┘ │ │
│  │                                                                       │ │
│  │  ┌─────────────────────────────────────────────────────────────────┐ │ │
│  │  │ trading/discovery.rs  [SERVICE DISCOVERY]                       │ │ │
│  │  │ ───────────────────────────────────────────────────────────────  │ │ │
│  │  │ • RouterDiscovery: Discover strategies marked as ready          │ │ │
│  │  │   - poll() -> Vec<DiscoveryEvent>                               │ │ │
│  │  │   - strategy_endpoints(strategy_id) -> StrategyEndpoints        │ │ │
│  │  │ • StrategyId: Unique strategy identifier                        │ │ │
│  │  │ • DiscoveryEvent: Added { strategy, orders_out } | Removed      │ │ │
│  │  └─────────────────────────────────────────────────────────────────┘ │ │
│  │                                                                       │ │
│  │ Uses: ipc::BidirectionalChannel, bus::is_ready, bus::mark_ready     │ │
│  └───────────────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────────┐
│                    FOUNDATION MODULES                                       │
│                                                                             │
│  • core/       - Queue (SWMR), Writer, Reader, Segment management          │
│  • ipc/        - Generic IPC patterns (BidirectionalChannel, PubSub,       │
│                  FanIn) - Pure infrastructure, no domain knowledge          │
│  • trading/    - Trading domain abstractions (StrategyChannel,             │
│                  RouterChannel, RouterDiscovery) - Built on IPC patterns   │
│  • bus/        - Process orchestration (SubscriberDiscovery, ready,        │
│                  lease, registration) - Generic coordination primitives    │
│  • storage/    - Archive storage with block compression                    │
│  • layout/     - Filesystem path conventions (generic infrastructure)      │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Module Interaction Patterns

### Pattern 1: Sequence Validation (Shared Utility)
```
Application
    │
    ▼
[ReplayEngine] ──uses──► [SequenceValidator] ◄──uses── [SzseL3Engine]
                              │
                         (single source of truth)
```

### Pattern 2: ETL Feature Processing (Abstraction via Traits)
```
Application
    │
    ▼
[Extractor<F, S>]
    │
    ├──► [FeatureSet] ──implements──► [GlobalTime, MidPrice, SpreadBps, ...]
    │         │
    │         └──uses──► [OrderBook trait] ◄──impl── [L2Book]
    │         └──uses──► [BookUpdate trait] ◄──impl── [BookEvent]
    │
    └──► [RowSink] ──implements──► [ParquetSink]
```

### Pattern 3: Protocol Serialization (Centralized Logic)
```
[Refinery]
    │
    └──uses──► [protocol::serialization::write_l2_snapshot()]
                    │
                    └──serializes──► [BookEventHeader + L2Snapshot + Levels]
```

### Pattern 4: Trading Domain (Layered Architecture)
```
Strategy Process                      Router Process
      │                                     │
      ▼                                     ▼
[StrategyChannel]                   [RouterChannel]
      │                                     │
      └──────uses──────► [BidirectionalChannel] ◄──────uses──────┘
                                │
                         (Generic IPC layer)

Router Process (Discovery)
      │
      ▼
[RouterDiscovery] ──scans──► [strategy_orders_out_path/READY]
      │                               │
      │                        (readiness marker)
      ▼
[DiscoveryEvent::Added] ──contains──► [StrategyId, orders_out path]
```

Architecture Layers:
```
┌───────────────────────────────────────────────┐
│  Application (Strategy/Router binaries)       │
├───────────────────────────────────────────────┤
│  trading/ (StrategyChannel, RouterChannel)    │  ← Domain Layer
├───────────────────────────────────────────────┤
│  ipc/ (BidirectionalChannel, PubSub, FanIn)   │  ← Generic Infrastructure
├───────────────────────────────────────────────┤
│  core/ (Queue, Writer, Reader)                │  ← Queue Primitives
└───────────────────────────────────────────────┘
```

Key Design Principles:
- **Domain Separation**: trading/ contains business logic, ipc/ is reusable
- **Zero-Cost Abstractions**: Trading wrappers compile to direct queue operations
- **Type Safety**: StrategyChannel vs RouterChannel prevent endpoint confusion
- **Composition**: Complex patterns built from simple primitives
  - Example: N-to-1 router hub = HashMap<StrategyId, RouterChannel> + FanInReader


## Cross-Cutting Concerns

### Testing Strategy
```
Unit Tests:
  • sequencer/mod.rs: 7 tests (sequence validation logic)
  • etl/features.rs: 8 tests (isolated feature logic using mocks)
  • protocol/mod.rs: 11 tests (serialization correctness)

Integration Tests:
  • Full pipeline tests (replay → features → sink)
  • SZSE L3 reconstruction tests (end-to-end)
```

### Error Handling
```
All modules use Result<T> with anyhow::Error:
  • Sequence gaps: GapPolicy determines behavior
  • Decode errors: Skip or fail based on policy
  • Protocol errors: Clear error messages with context
```

### Configuration
```
Policies control behavior:
  • GapPolicy: Panic | Quarantine | Ignore
  • ReplayPolicy: Contains gap policy
  • ReconstructPolicy: Contains gap + decode + unknown_order policies
```
