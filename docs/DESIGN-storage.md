# Chronicle Unified Storage Architecture

## 1. Executive Summary

The Chronicle Storage Architecture resolves the tension between **HFT Latency** (Write Speed) and **Quantitative Research** (Read Usability). It establishes the **Stream** as the singular source of truth while leveraging tiered storage physics.

### Core Principles
1.  **Unified Format (The Truth):** The `.q` format is the canonical record across all tiers. Simulation fidelity is absolute.
2.  **Tiered Physics:** Storage medium determines access method (`mmap` vs `read`), but not the logical data structure.
3.  **Path Resolution:** The filesystem hierarchy, combined with a Tier Priority logic, serves as the global index.

---

## 2. The Topology (Three Zones)

| Zone | Component | Role | Medium | Access | Latency |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **Zone 1** | **`chronicle-feed`** | Firehose | `/dev/shm` | `mmap` (Raw) | **< 1 µs** |
| **Zone 2** | **`chronicle-etl`** | Refinery | `/mnt/nvme-warm` | `mmap` (Clean) | **~100 µs** |
| **Zone 3** | **`chronicle-storage`** | Archive | `/mnt/hdd` | `read` (Compressed) | **~10 ms** |

---

## 3. Physical Layout & Schema

**Layout Strategy:** `v1/{Venue}/{Symbol}/{Date}/...`

```text
/data/
├── v1/
│   ├── binance-perp/
│   │   ├── btc-usdt/
│   │   │   ├── 2024-01-24/           # UTC Partition
│   │   │   │   ├── book.q            # Hot/Warm: Raw Segment
│   │   │   │   ├── meta.json         # Provenance & completeness
│   │   │   │   └── index/
│   │   │   └── 2024-01-01/
│   │   │       ├── book.q.zst        # Cold: Seekable Zstd
│   │   │       └── book.q.zst.idx    # Cold: Zstd Seek Table
```

### Key Decision: Partitioning
*   **Strategy:** Partition by **Exchange Time (UTC)**.
*   **Cutover:** The Refinery maintains open writers for `CurrentDay` and `PreviousDay` (for 1 hour) to handle stragglers.
*   **Late Data:** Data arriving >1h late is rejected or routed to a `late` stream to preserve partition immutability.

### Key Decision: Symbol Normalization (SCD Type 2)
We assume a data lake `dim_symbol` table with SCD2 semantics.
*   **Canonical Identity:** Use a stable `symbol_id` (surrogate key). Symbol strings are attributes that can change.
*   **As-of Resolution:** `chronicle-etl` resolves `(venue, raw_symbol, event_time)` against `dim_symbol` and attaches:
    *   `symbol_id` (stable)
    *   `symbol_code` (canonical string as-of event time)
    *   `symbol_version_id` (specific SCD row, optional)
*   **Path Choice:** Directory `{Symbol}` uses `symbol_code` as-of that date for readability.
*   **Analytics Join:** Queries join on `symbol_id` and filter `dim_symbol` by `[valid_from, valid_to)` using event time.

### Metadata: Provenance & Completeness
Each partition includes a `meta.json` with immutable metadata:
*   `symbol_id`, `symbol_version_id`, `symbol_code`
*   `source` (ingest pipeline, source file, or feed)
*   `ingest_time`, `event_time_range`, `completeness` (gap flags)
*   `late_policy` (e.g., "reject", "late_stream")

---

## 4. Compression & Access Strategy

### Decision: Seekable Zstd
We reject monolithic compression. Cold data uses **Zstd Seekable Format**.
*   **Structure:** Independent compressed frames (default 1 MiB uncompressed blocks).
*   **Indexing:** `book.q.zst.idx` stores a header and a jump table:
    *   Header: magic `QZSTIDX1`, version, block_size, frame_count.
    *   Table entries: `u64 uncompressed_offset`, `u64 compressed_offset`, `u32 compressed_size`, `u32 uncompressed_size`.
*   **Benefit:** Enables `O(1)` random access (Seek) even in Cold Storage.

### Decision: Path Priority Resolution
We reject a central consistent manifest database.
*   **Logic:** Readers resolve in strict priority order: `Hot -> Warm -> Cold -> RemoteCold`.
*   **Consistency:** Moves are `Copy -> Fsync -> Atomic Rename -> Delete`.
*   **No Double-Read:** Readers stop at the first tier hit; they never read multiple tiers for the same partition.
*   **Resilience:** If a file vanishes during a tier transition, readers retry and re-resolve.

---

## 5. Components & Lifecycle

### A. `chronicle-etl` (The Refinery)
*   **Input:** Raw Zone 1 Stream.
*   **Logic:**
    *   Reconstructs Order Book state.
    *   Injects Snapshots (every 60s).
    *   Partitions data by **Exchange Time** (UTC).
*   **Output:** Canonical Zone 2 Stream.

### B. `chronicle-storage` (The Lifecycle Manager)
*   **TierManager:**
    *   `Warm -> Cold`: Triggered by Age (>24h). Transcodes `.q` -> `.q.zst` (Seekable).
    *   `Cold -> Glacier`: Triggered by Age (>1y). Uploads to Object Storage.
*   **Deriver (Optional):**
    *   Can generate `derived/book.parquet` during the Cold move for pure analytics loads.

#### Object Storage Move Protocol (No Central Manifest)
Object storage has no atomic rename, so we use a local stub:
1. Upload `book.q.zst` and `book.q.zst.idx` to remote path.
2. Verify size/checksum (ETag or SHA256).
3. Write `book.q.zst.remote.json` locally with `remote_uri`, `etag`, `size`.
4. Delete local `book.q.zst` and `book.q.zst.idx`.
Readers treat `*.remote.json` as `RemoteCold` tier.

### C. `chronicle-core` (The Universal Reader)
*   **Smart Reader:**
    *   Detects `.q` vs `.q.zst` via magic bytes/extension.
    *   Transparently handles decompression blocks.
    *   Abstracts the Tier location from the user.

---

## 6. Schema Evolution

*   **Binary Compatibility:** `chronicle-protocol` uses `u16 type_id`. New message versions get new IDs (e.g., `BookEventV2`).
*   **Layout Versioning:** The `/v1/` root prefix allows future changes to the directory hierarchy without breaking existing paths.
