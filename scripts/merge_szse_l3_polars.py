#!/usr/bin/env python3
import argparse
import os
import sys
import tempfile

import polars as pl

EVENT_HEADER = [
    "ChannelNo",
    "ApplSeqNum",
    "Event",
    "Symbol",
    "OrderID",
    "BidOrderID",
    "OfferOrderID",
    "Side",
    "Price",
    "Qty",
    "Amt",
    "OrdType",
    "ExecType",
    "TransactTime",
    "SendingTime",
    "Source",
]

ORDER_SCHEMA = {
    "OrderQty": pl.Int64,
    "OrdType": pl.Utf8,
    "TransactTime": pl.Int64,
    "ExpirationDays": pl.Int32,
    "Side": pl.Int8,
    "ApplSeqNum": pl.Int64,
    "Contactor": pl.Int64,
    "SendingTime": pl.Int64,
    "Price": pl.Float64,
    "ChannelNo": pl.Int32,
    "ExpirationType": pl.Int32,
    "ContactInfo": pl.Int64,
    "ConfirmID": pl.Int64,
}

TICK_SCHEMA = {
    "ApplSeqNum": pl.Int64,
    "BidApplSeqNum": pl.Int64,
    "SendingTime": pl.Int64,
    "Price": pl.Float64,
    "ChannelNo": pl.Int32,
    "Qty": pl.Int64,
    "OfferApplSeqNum": pl.Int64,
    "Amt": pl.Float64,
    "ExecType": pl.Utf8,
    "TransactTime": pl.Int64,
}


def symbol_from_path_expr():
    # Extract 6-digit symbol from file path.
    return pl.col("path").str.extract(r"([0-9]{6})\.csv$")


def build_order_lazy(order_glob, encoding, ignore_errors):
    lf = pl.scan_csv(
        order_glob,
        schema_overrides=ORDER_SCHEMA,
        include_file_paths="path",
        low_memory=True,
        encoding=encoding,
        ignore_errors=ignore_errors,
    )
    return (
        lf.with_columns(
            [
                pl.lit("ORDER").alias("Event"),
                symbol_from_path_expr().alias("Symbol"),
                pl.col("ApplSeqNum").alias("OrderID"),
                pl.lit(None, dtype=pl.Int64).alias("BidOrderID"),
                pl.lit(None, dtype=pl.Int64).alias("OfferOrderID"),
                pl.col("Side").cast(pl.Int8).alias("Side"),
                pl.col("Price").alias("Price"),
                pl.col("OrderQty").alias("Qty"),
                pl.lit(None, dtype=pl.Float64).alias("Amt"),
                pl.col("OrdType").alias("OrdType"),
                pl.lit(None, dtype=pl.Utf8).alias("ExecType"),
                pl.col("TransactTime").alias("TransactTime"),
                pl.col("SendingTime").alias("SendingTime"),
                pl.lit("order").alias("Source"),
            ]
        )
        .select(EVENT_HEADER)
    )


def build_tick_lazy(tick_glob, encoding, ignore_errors):
    lf = pl.scan_csv(
        tick_glob,
        schema_overrides=TICK_SCHEMA,
        include_file_paths="path",
        low_memory=True,
        encoding=encoding,
        ignore_errors=ignore_errors,
    )
    event_expr = (
        pl.when(pl.col("ExecType") == "F")
        .then(pl.lit("TRADE"))
        .when(pl.col("ExecType") == "4")
        .then(pl.lit("CANCEL"))
        .otherwise(pl.lit("TICK"))
    )
    return (
        lf.with_columns(
            [
                event_expr.alias("Event"),
                symbol_from_path_expr().alias("Symbol"),
                pl.lit(None, dtype=pl.Int64).alias("OrderID"),
                pl.col("BidApplSeqNum").alias("BidOrderID"),
                pl.col("OfferApplSeqNum").alias("OfferOrderID"),
                pl.lit(None, dtype=pl.Int8).alias("Side"),
                pl.col("Price").alias("Price"),
                pl.col("Qty").alias("Qty"),
                pl.col("Amt").alias("Amt"),
                pl.lit(None, dtype=pl.Utf8).alias("OrdType"),
                pl.col("ExecType").alias("ExecType"),
                pl.col("TransactTime").alias("TransactTime"),
                pl.col("SendingTime").alias("SendingTime"),
                pl.lit("tick").alias("Source"),
            ]
        )
        .select(EVENT_HEADER)
    )


def split_sorted_csv(sorted_path, out_dir):
    os.makedirs(out_dir, exist_ok=True)
    handles = {}
    header = None
    current_channel = None
    current_handle = None

    with open(sorted_path, "r", newline="") as f:
        for line_num, line in enumerate(f, start=1):
            line = line.rstrip("\n")
            if line_num == 1:
                header = line
                continue
            if not line:
                continue
            channel = line.split(",", 1)[0]
            if channel != current_channel:
                current_channel = channel
                if channel not in handles:
                    path = os.path.join(out_dir, f"channel_{channel}.csv")
                    h = open(path, "w", newline="")
                    h.write(header + "\n")
                    handles[channel] = h
                current_handle = handles[channel]
            current_handle.write(line + "\n")

    for h in handles.values():
        h.close()


def list_csv_files(dir_path, limit=None):
    files = sorted(
        os.path.join(dir_path, name)
        for name in os.listdir(dir_path)
        if name.endswith(".csv")
    )
    if limit is not None:
        files = files[:limit]
    return files


def read_channel_no(path):
    with open(path, "r", encoding="utf-8", errors="ignore") as f:
        header = f.readline().strip().split(",")
        try:
            idx = header.index("ChannelNo")
        except ValueError:
            raise RuntimeError(f"ChannelNo not found in header: {path}")
        for line in f:
            line = line.strip()
            if not line:
                continue
            parts = line.split(",")
            if idx >= len(parts):
                continue
            return int(parts[idx])
    return None


def group_files_by_channel(files):
    mapping = {}
    for path in files:
        channel = read_channel_no(path)
        if channel is None:
            continue
        mapping.setdefault(channel, []).append(path)
    return mapping


def main():
    parser = argparse.ArgumentParser(
        description="Polars-based merge/sort of SZSE order+tick CSVs."
    )
    parser.add_argument("--order-dir", required=True, help="Directory with order CSV files")
    parser.add_argument("--tick-dir", required=True, help="Directory with tick CSV files")
    parser.add_argument("--out", default=None, help="Output path for merged sorted CSV")
    parser.add_argument("--out-dir", default=None, help="Output directory for per-channel CSVs")
    parser.add_argument("--tmp", default=None, help="Temp directory for intermediate files")
    parser.add_argument("--limit-files", type=int, default=None, help="Limit files per dir (testing)")
    parser.add_argument("--encoding", default="utf8-lossy", help="CSV encoding for Polars")
    parser.add_argument("--ignore-errors", action="store_true", help="Skip rows with parse errors")
    args = parser.parse_args()

    if args.out_dir and args.out:
        raise RuntimeError("Use either --out or --out-dir, not both.")
    if not args.out_dir and not args.out:
        raise RuntimeError("Specify --out (single file) or --out-dir (per-channel).")

    if args.out_dir:
        order_files = list_csv_files(args.order_dir, args.limit_files)
        tick_files = list_csv_files(args.tick_dir, args.limit_files)
        order_by_channel = group_files_by_channel(order_files)
        tick_by_channel = group_files_by_channel(tick_files)
        channels = sorted(set(order_by_channel) | set(tick_by_channel))

        os.makedirs(args.out_dir, exist_ok=True)
        for channel in channels:
            frames = []
            order_list = order_by_channel.get(channel, [])
            tick_list = tick_by_channel.get(channel, [])
            if order_list:
                frames.append(
                    build_order_lazy(order_list, args.encoding, args.ignore_errors)
                )
            if tick_list:
                frames.append(
                    build_tick_lazy(tick_list, args.encoding, args.ignore_errors)
                )
            if not frames:
                continue
            merged = pl.concat(frames, how="vertical").sort(
                ["ChannelNo", "ApplSeqNum"]
            )
            out_path = os.path.join(args.out_dir, f"channel_{channel}.csv")
            merged.sink_csv(out_path)
    else:
        order_glob = os.path.join(args.order_dir, "*.csv")
        tick_glob = os.path.join(args.tick_dir, "*.csv")
        if args.limit_files is not None:
            order_glob = list_csv_files(args.order_dir, args.limit_files)
            tick_glob = list_csv_files(args.tick_dir, args.limit_files)
        order_lf = build_order_lazy(order_glob, args.encoding, args.ignore_errors)
        tick_lf = build_tick_lazy(tick_glob, args.encoding, args.ignore_errors)
        merged = pl.concat([order_lf, tick_lf], how="vertical").sort(
            ["ChannelNo", "ApplSeqNum"]
        )
        merged.sink_csv(args.out)


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        print(f"error: {exc}", file=sys.stderr)
        sys.exit(1)
