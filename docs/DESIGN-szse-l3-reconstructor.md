# SZSE A-Share L3 (MBO) Order Book Reconstructor

This document defines a practical, deterministic blueprint for reconstructing a Shenzhen Stock Exchange (SZSE) A-share order-by-order (Market-by-Order, "L3") limit order book from exchange feeds. It is intended as a mapping guide for integrating SZSE tick-by-tick order/trade events into `chronicle-rs`'s protocol and persistence model.

This is a **design blueprint**, not a definitive exchange specification. Exact field names, values, and behaviors must be validated against your licensed SZSE data feed documentation.

## Goals

- Deterministically reconstruct full L3 book state from order + trade events.
- Detect gaps using per-channel sequence continuity.
- Preserve strict ordering required for local state integrity.
- Support auction phase handling and SZSE-specific quirks (e.g., price=0/-1).
- Define how to map events into `chronicle-rs` protocol structures.

## Non-Goals

- Defining vendor-specific feed schemas or licensing terms.
- Providing a full matching engine. We only reconstruct the official book state implied by feed events.
- Estimating queue position or hidden order behavior not exposed by the feed.

## Required Feed Semantics (Minimum Viable Set)

You need **both** order events and trade events, plus a **per-channel sequence number**.

Required message semantics:

- **Order Add/New**: order_id, side, price, qty, exchange_ts, seq
- **Order Cancel/Delete**: order_id, canceled_qty (or remaining_qty), exchange_ts, seq
- **Trade/Execution**: price, qty, exchange_ts, seq, and **resting order linkage**
  - best case: bid_order_id + ask_order_id
  - minimum: resting order_id + aggressor side

If the feed only provides market-by-price (MBP) or snapshots, you cannot reconstruct L3 uniquely.

## Quant360 Raw Data Mapping (Inferred)

This section maps the observed CSV fields from:

- `order_new_STK_SZ_YYYYMMDD.7z` (per-symbol order events)
- `tick_new_STK_SZ_YYYYMMDD.7z` (per-symbol trade/cancel events)

### `order_new_STK_SZ_YYYYMMDD/<symbol>.csv`

Header (observed):

```
OrderQty,OrdType,TransactTime,ExpirationDays,Side,ApplSeqNum,Contactor,SendingTime,Price,ChannelNo,ExpirationType,ContactInfo,ConfirmID
```

Inferred semantics:

- `OrderQty`: order size
- `OrdType`: observed values `1`, `2`, `U`
  - `2` appears to be standard limit orders (price always non-zero)
  - `1` appears market-like (price often `0.000`)
  - `U` is rare; treat as special order type and preserve as a flag
- `TransactTime`: exchange timestamp (format `YYYYMMDDhhmmssSSS`, 17 digits)
- `Side`: `1` = buy, `2` = sell
- `ApplSeqNum`: per-channel sequence number; also acts as **order_id**
  - Used as `BidApplSeqNum` / `OfferApplSeqNum` in tick data
  - Monotonic within the file (non-decreasing)
- `SendingTime`: feed send time (same 17-digit format)
- `Price`: decimal price; `0.000` appears for market-like orders
- `ChannelNo`: channel identifier (constant within a symbol file)

### `tick_new_STK_SZ_YYYYMMDD/<symbol>.csv`

Header (observed):

```
ApplSeqNum,BidApplSeqNum,SendingTime,Price,ChannelNo,Qty,OfferApplSeqNum,Amt,ExecType,TransactTime
```

Inferred semantics:

- `ApplSeqNum`: per-channel sequence number for tick events
  - Monotonic within the file (non-decreasing)
  - Range aligns with `order_new` `ApplSeqNum` values, indicating a shared sequence space
- `BidApplSeqNum` / `OfferApplSeqNum`: order_ids from the order stream
- `ExecType`: observed values `F` and `4`
  - `F` = trade (both bid/offer IDs present, `Price` and `Qty` non-zero)
  - `4` = cancel (only one side ID present, `Price` and `Amt` are zero)
- `Qty`: trade size or canceled size (depending on `ExecType`)
- `Amt`: trade notional (`Price * Qty`) for `F`, otherwise `0.000`

### Ordering / Merge Rule

The order and tick files are separate, but their `ApplSeqNum` ranges overlap and align by `ChannelNo`. This strongly suggests a shared sequence space across order and trade events.

Recommended ordering key:

```
(ChannelNo, ApplSeqNum)
```

Important nuance:

- When files are **symbol-filtered**, `ApplSeqNum` gaps are expected (events for other symbols are missing).
- You can still use `ApplSeqNum` to **merge order + tick events for the same symbol**, but you cannot treat gaps as packet loss unless you ingest the full channel stream.

## SZSE-Specific Considerations

1) **Market orders with price=0 or price=-1**
   - Some SZSE tick-by-tick feeds use 0 or -1 to represent a market order type.
   - Do not treat these as real limit prices; treat price as "unknown / derived".
   - The execution price is inferred at trade time.

2) **Cancellation signaling**
   - Some cancellation indications appear inside trade-like messages.
   - A cancel may carry price=0 while the original order price is stored only in the order book state.
   - Always join back to `orders_by_id` for price/side/value.

3) **Auction phase rules**
   - Call auction and continuous auction have different matching rules and allowed cancels.
   - Book reconstruction should be phase-aware to avoid misinterpreting valid feed patterns.
   - Typical A-share phases:
     - Opening call auction: 09:15–09:25
     - Continuous: 09:30–11:30 and 13:00–14:57
     - Closing call auction: 14:57–15:00
- Expect fewer cancels in restricted windows (e.g., 09:20–09:25 and 14:57–15:00).

## Auction Phase Handling (Required)

SZSE A-shares operate with distinct phases that affect message interpretation.
Your reconstructor should be phase-aware even if the feed does not explicitly tag phases.

### Phase Schedule (Local Exchange Time)

- **Opening call auction**: 09:15–09:25
- **No-cancel window**: 09:20–09:25
- **Continuous auction**: 09:30–11:30 and 13:00–14:57
- **Closing call auction**: 14:57–15:00
- **No-cancel window**: 14:57–15:00

### Recommended Phase Rules

1) **Phase classification**
   - Derive phase from `TransactTime` if no explicit phase flag is provided.
   - Keep a per-symbol `phase_state` based on the event timestamp.

2) **Order add during call auctions**
   - Accept all adds.
   - If `PRICE_IS_MARKET` (price=0/-1), keep in `orders_by_id` but do not place into price ladder.

3) **Cancel handling in no-cancel windows**
   - If a cancel arrives in the no-cancel window, do not assume data corruption.
   - Mark with `CANCEL_RESTRICTED_WINDOW` flag and log; some cancels may still appear due to late reporting or special cases.

4) **Trade handling during call auctions**
   - Trades may cluster at the match time (09:25, 15:00).
   - Apply trades normally, but do not require continuous-auction style queue behavior.

5) **Validation logic**
   - If you validate against snapshots, compare only after call auction completes (09:25) and during continuous trading.
   - Expect transient mismatches during auction accumulation.

### Implementation Notes

- Phase detection can be a pure function of timestamp; no additional feed fields required.
- Store the phase in `flags` (e.g., `AUCTION_OPEN` / `AUCTION_CLOSE`) for downstream analytics.
- If the feed later exposes explicit phase tags, prefer those over timestamp heuristics.

## Core Reconstruction State

Maintain the following in memory and optionally persist:

- `orders_by_id: order_id -> {side, price, remaining_qty, ts, flags}`
- `book_by_price`: per side, price -> FIFO queue (or aggregate depth if not tracking queue position)
- `phase_state`: current auction phase for the symbol

Recommended persistence checkpoints (for fast replay or crash recovery):

- Periodic snapshot of `orders_by_id` and top-N `book_by_price`
- Sequence cursor per channel

## Matching vs Reconstruction (FIFO Use)

The reconstructor does **not** simulate the exchange matching engine. FIFO queues are used
only to preserve book order and queue position metadata.

- **Continuous auction:** price-time priority implies FIFO within a price level, so FIFO
  queues are appropriate for **book storage** and cancel handling.
- **Call auctions (opening/closing):** matching is batch-based at a single price, so FIFO
  **does not** represent execution priority. Keep FIFO order for storage, but apply trades
  strictly as reported by the feed.
- **All phases:** trades are authoritative; do not infer executions from the book.
  If trade linkage to order IDs is missing, treat it as a depth-only update and avoid
  simulated matching to prevent drift.

## Strict Sequencing (Gap Detection)

Events must be processed strictly in **exchange sequence order**.

- Key: `(channel_id, seq)` is authoritative.
- If `seq` is discontinuous:
  - mark the book "invalid"
  - backfill via retransmission or vendor replay
  - do not continue live reconstruction without repair

Timestamp is secondary (only used for debugging or tiebreaks if required by feed).

## Event Handling Rules (State Machine)

Process each event in order:

1) **Order Add/New**
   - Insert into `orders_by_id`
   - Enqueue at `book_by_price[side][price]`

2) **Order Cancel/Delete**
   - Look up order_id
   - Decrement remaining_qty, remove if fully canceled
   - Update `book_by_price` accordingly

3) **Trade/Execution**
   - Identify resting order(s) via linked order_id(s)
   - Decrement remaining_qty for each resting order
   - Remove from book if remaining_qty == 0

If a trade references an unknown order_id, treat as a gap or mis-ordering condition.

## Recommended Validation Loop

If snapshot data is available:

- Compare derived top-of-book and depth aggregates against official snapshots.
- Flag drift immediately; trigger re-sync if snapshot mismatch persists.

## Mapping to `chronicle-rs` Protocol

Current protocol structures (see `src/protocol/mod.rs`) provide a partial match:

- `BookEventHeader` gives `seq`/`native_seq` ordering fields.
- `BookMode::L3` indicates L3 event payloads.
- `L3Diff` carries per-order fields: `order_id`, `side`, `price`, `size`, `action`.
- `Trade` carries buyer/seller order IDs.

### Minimum Mapping

For each feed event, emit:

- `TypeId::BookEvent` + `BookEventHeader` + `L3Diff` for order add/cancel/modify
- `TypeId::Trade` for executions, with order linkage populated

### Suggested Extensions (Optional but Strongly Recommended)

To support full SZSE semantics, consider:

1) **Typed L3 action enum**
   - Define explicit action values: Add / Cancel / Modify / Execute
2) **L3 trade payload or explicit ordering**
   - Either include L3 trades inside `BookEvent`, or
   - Add a `book_seq` to `Trade` so execution ordering is explicit
3) **Flags field**
   - Bits for market-order price=0/-1, auction phase, cancel-restriction windows

## Generic L3Event Schema (Venue-Agnostic)

This is the normalized event model used by the reconstructor. It is designed to be
venue-neutral, with a separate mapping layer for SZSE specifics.

### Envelope

```
L3Envelope {
  venue_id          // exchange/venue identifier
  market_id         // symbol/market identifier (or hash)
  channel_id        // feed channel
  seq               // per-channel sequence number
  exchange_ts_ns    // exchange timestamp
  sending_ts_ns     // feed send timestamp (optional)
  event_type        // enum: OrderAdd, OrderCancel, OrderModify, Trade, Reset, Heartbeat
  flags             // bitset: PRICE_IS_MARKET, AUCTION_OPEN, AUCTION_CLOSE, ...
}
```

### Payloads

```
L3OrderAdd {
  order_id
  side              // Buy/Sell/Unknown
  price             // 0 allowed if market-like
  qty
  ord_type          // raw or normalized order type
}

L3OrderCancel {
  order_id
  side              // optional
  cancel_qty
  reason            // Cancel/Expire/System/Unknown (optional)
}

L3OrderModify {
  order_id
  new_price
  new_qty
  reason            // Replace/Modify/Unknown
}

L3Trade {
  price
  qty
  amt
  bid_order_id      // optional
  ask_order_id      // optional
  aggressor_side    // optional
}
```

### Flags / Enums (generic)

- `EventType`: OrderAdd | OrderCancel | OrderModify | Trade | Reset | Heartbeat
- `Side`: Buy | Sell | Unknown
- `Flags` (bitset):
  - `PRICE_IS_MARKET` (price=0 or -1 in raw feed)
  - `AUCTION_OPEN`
  - `AUCTION_CLOSE`
  - `CANCEL_RESTRICTED_WINDOW`
  - `RAW_HAS_BID_ID`
  - `RAW_HAS_ASK_ID`

## SZSE Mapping (Venue-Specific)

This section maps the Quant360 SZSE files into the generic schema.

### Order stream (`order_new_STK_SZ_YYYYMMDD`)

Map each row to `OrderAdd`:

- `seq` = `ApplSeqNum`
- `order_id` = `ApplSeqNum`
- `side` = `Side` (1=Buy, 2=Sell)
- `price` = `Price`
- `qty` = `OrderQty`
- `ord_type` = `OrdType` (raw value: "1", "2", "U")
- `flags`: set `PRICE_IS_MARKET` if `Price == 0` or `OrdType == "1"`

### Tick stream (`tick_new_STK_SZ_YYYYMMDD`)

Map each row by `ExecType`:

- `ExecType == "F"` => `Trade`
  - `seq` = `ApplSeqNum`
  - `price` = `Price`
  - `qty` = `Qty`
  - `amt` = `Amt`
  - `bid_order_id` = `BidApplSeqNum` (may be 0)
  - `ask_order_id` = `OfferApplSeqNum` (may be 0)
  - `flags`: `RAW_HAS_BID_ID` if `BidApplSeqNum != 0`, `RAW_HAS_ASK_ID` if `OfferApplSeqNum != 0`

- `ExecType == "4"` => `OrderCancel`
  - `seq` = `ApplSeqNum`
  - `order_id` = `BidApplSeqNum` if non-zero else `OfferApplSeqNum`
  - `cancel_qty` = `Qty`
  - `flags`: `PRICE_IS_MARKET` if `Price == 0` (raw cancel convention)

### Ordering rules

- Merge order + tick events by `(ChannelNo, ApplSeqNum)` for a full channel stream.
- Gaps only indicate missing data if you ingest **all symbols** for that channel.

## Integration Checklist

- [ ] Confirm feed fields for order add, cancel, trade, sequence.
- [ ] Define mapping into `L3Diff` and `Trade`.
- [ ] Implement gap detector and retransmission hooks.
- [ ] Implement phase-aware rule handling (auction vs continuous).
- [ ] Add snapshot validation if snapshots are available.
- [ ] Persist checkpoint snapshots periodically for recovery.

## Open Questions for Feed-Specific Mapping

Provide the following details to finalize mapping:

- Exact field names for order add, cancel, trade, and sequence
- Whether trade events include both sides' order IDs
- How auction phases are indicated (if at all)
- The canonical meaning of price=0/-1 in your feed
