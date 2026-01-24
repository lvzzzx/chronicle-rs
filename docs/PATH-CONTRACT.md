# Chronicle Path Contract (v1)

This document defines the **filesystem contract** between the live IPC layer
(`chronicle-bus`), the fan-out processor, and the archival tiering layer
(`chronicle-storage`). It is intentionally concrete and versioned.

## 1) Root Separation (Option A)

**IPC Root (live system):**

```
/var/lib/hft_bus/<bus>/
```

**Archive Root (tiered storage):**

```
/var/lib/chronicle-archive/
```

The two roots must be independent. IPC is optimized for ultra-low latency and
ephemeral data. Archive is optimized for stable partitioning, compression, and
long retention.

## 2) IPC Layout (chronicle-bus)

The IPC layout root is owned by `chronicle-layout::IpcLayout`, which provides
both `StreamsLayout` (data plane) and `OrdersLayout` (control plane).

```
/var/lib/hft_bus/<bus>/
  streams/
    raw/<venue>/queue/
      000000000.q
      000000001.q
  orders/
    queue/<strategy_id>/orders_out/
    queue/<strategy_id>/orders_in/
```

### IPC Naming Rules
- `<venue>` is a stable venue identifier (e.g. `binance-perp`).
- `<symbol>` is the canonical symbol code for the event-time date.
- `<stream>` is a logical stream name (e.g. `trades`, `book`, `l2`).
- IPC queues always live under a `queue/` directory.
- Segment filenames are `{:09}.q` (chronicle-core standard).

### Optional Live Clean Streams
If you need live, demultiplexed streams for IPC consumers, add:

```
/var/lib/hft_bus/<bus>/
  streams/
    clean/<venue>/<symbol>/<stream>/queue/
      000000000.q
      000000001.q
```

If you are **archive-only**, do not create `streams/clean/...`.

## 3) Archive Layout (chronicle-storage)

The archive layout is owned by `chronicle-storage` and is versioned under `v1/`.

```
/var/lib/chronicle-archive/
  v1/<venue>/<symbol>/<date>/<stream>/
    000000000.q
    000000001.q
    meta.json
    000000000.q.zst
    000000000.q.zst.idx
    000000000.q.zst.remote.json
```

### Archive Naming Rules
- `<date>` is UTC partition date, format `YYYY-MM-DD`.
- The archive path is **partitioned by event-time**.
- Segment names remain `{:09}.q` so readers can reuse core logic.
- Compression outputs use the same stem:
  - `000000000.q.zst`
  - `000000000.q.zst.idx`
  - optional `000000000.q.zst.remote.json`

## 4) Fan-out Processor Contract

The fan-out processor is the bridge between IPC and archive.

### Inputs (IPC)
```
/var/lib/hft_bus/<bus>/streams/raw/<venue>/queue/
```

### Outputs (Archive)
```
/var/lib/chronicle-archive/v1/<venue>/<symbol>/<date>/<stream>/
```

### Routing Contract
- Each input message must be mapped to `(symbol, stream)` by header/type.
- Event time determines the `<date>` partition in the archive.
- Fan-out must preserve per-stream ordering.
- The raw queue is the canonical source of truth; fan-out is re-derivable.
- Optional: if you enable live clean streams, fan-out may also write to
   `/var/lib/hft_bus/<bus>/streams/clean/.../queue/`.

## 5) Tiering Contract

`chronicle-storage` only operates inside the Archive Root.

- **Hot -> Cold:** `.q` → `.q.zst` + `.q.zst.idx` (seekable)
- **Cold -> Remote:** copy to remote root and write `.q.zst.remote.json`
- All moves are `copy + fsync + rename + delete` to avoid partials.

## 6) Metadata Contract (`meta.json`)

`meta.json` is optional but recommended. At minimum:

```
{
  "venue": "binance-perp",
  "symbol_code": "btc-usdt",
  "ingest_time_ns": 0,
  "event_time_range": [0, 0],
  "completeness": "sealed"
}
```

## 7) Configuration Contract

Recommended configuration keys for services:
- `bus_root`: `/var/lib/hft_bus/<bus>`
- `archive_root`: `/var/lib/chronicle-archive`
- `remote_root`: optional path or URI for cold storage offload

## 8) Examples

### Raw queue (IPC)
```
/var/lib/hft_bus/prod/streams/raw/binance-perp/queue/000000042.q
```

### Clean queue (IPC)
Optional (only if live clean streams are enabled):

```
/var/lib/hft_bus/prod/streams/clean/binance-perp/btc-usdt/trades/queue/000000007.q
```

### Archive (tiered)
```
/var/lib/chronicle-archive/v1/binance-perp/btc-usdt/2026-01-24/trades/000000123.q.zst
```

## 9) CSV / Third-Party Imports (Archive-Only)

Batch imports should bypass IPC and write **directly to archive**:

```
/var/lib/chronicle-archive/v1/<venue>/<symbol>/<date>/<stream>/
```

### Import Rules
- Partition by **event time** in UTC to determine `<date>`.
- Preserve per-stream ordering if possible. If not, set `meta.json.completeness`
  to indicate unordered or partial data.
- Write `meta.json` with source details (`provider`, `file_name`, ingest time).
