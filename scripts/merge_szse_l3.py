#!/usr/bin/env python3
import argparse
import csv
import os
import re
import shutil
import subprocess
import sys
import tempfile
from heapq import heappop, heappush

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


def run_7z_list(archive_path):
    cmd = ["7z", "l", "-slt", archive_path]
    proc = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if proc.returncode != 0:
        raise RuntimeError(f"7z list failed: {proc.stderr.strip()}")
    return proc.stdout.splitlines()


def list_csv_paths(archive_path):
    for line in run_7z_list(archive_path):
        if not line.startswith("Path = "):
            continue
        path = line[7:].strip()
        if path.endswith(".csv"):
            yield path


def stream_csv_rows(archive_path, inner_path):
    cmd = ["7z", "e", "-so", archive_path, inner_path]
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if proc.stdout is None:
        raise RuntimeError("Failed to open 7z stdout")
    reader = csv.DictReader(proc.stdout)
    for row in reader:
        yield row
    stderr = proc.stderr.read() if proc.stderr else ""
    ret = proc.wait()
    if ret != 0:
        raise RuntimeError(f"7z extract failed for {inner_path}: {stderr.strip()}")


def write_order_events(archive_path, inner_path, out_path, only_channel=None):
    symbol = os.path.splitext(os.path.basename(inner_path))[0]
    channel_seen = None
    row_count = 0
    with open(out_path, "w", newline="") as out_f:
        writer = csv.writer(out_f)
        writer.writerow(EVENT_HEADER)
        prev_seq = None
        for row in stream_csv_rows(archive_path, inner_path):
            channel = int(row["ChannelNo"])
            if only_channel is not None and channel != only_channel:
                continue
            if channel_seen is None:
                channel_seen = channel
            elif channel_seen != channel:
                raise RuntimeError(
                    f"Multiple ChannelNo values in {inner_path}: {channel_seen} vs {channel}"
                )
            seq = int(row["ApplSeqNum"])
            if prev_seq is not None and seq < prev_seq:
                raise RuntimeError(
                    f"Order stream not monotonic: {inner_path} seq {seq} < {prev_seq}"
                )
            prev_seq = seq
            writer.writerow(
                [
                    channel,
                    seq,
                    "ORDER",
                    symbol,
                    seq,
                    "",
                    "",
                    row["Side"],
                    row["Price"],
                    row["OrderQty"],
                    "",
                    row["OrdType"],
                    "",
                    row["TransactTime"],
                    row["SendingTime"],
                    "order",
                ]
            )
            row_count += 1
    if row_count == 0:
        os.remove(out_path)
        return None
    return channel_seen


def write_tick_events(archive_path, inner_path, out_path, only_channel=None):
    symbol = os.path.splitext(os.path.basename(inner_path))[0]
    channel_seen = None
    row_count = 0
    with open(out_path, "w", newline="") as out_f:
        writer = csv.writer(out_f)
        writer.writerow(EVENT_HEADER)
        prev_seq = None
        for row in stream_csv_rows(archive_path, inner_path):
            channel = int(row["ChannelNo"])
            if only_channel is not None and channel != only_channel:
                continue
            if channel_seen is None:
                channel_seen = channel
            elif channel_seen != channel:
                raise RuntimeError(
                    f"Multiple ChannelNo values in {inner_path}: {channel_seen} vs {channel}"
                )
            seq = int(row["ApplSeqNum"])
            if prev_seq is not None and seq < prev_seq:
                raise RuntimeError(
                    f"Tick stream not monotonic: {inner_path} seq {seq} < {prev_seq}"
                )
            prev_seq = seq
            exec_type = row["ExecType"]
            if exec_type == "F":
                event = "TRADE"
            elif exec_type == "4":
                event = "CANCEL"
            else:
                event = "TICK"
            writer.writerow(
                [
                    channel,
                    seq,
                    event,
                    symbol,
                    "",
                    row["BidApplSeqNum"],
                    row["OfferApplSeqNum"],
                    "",
                    row["Price"],
                    row["Qty"],
                    row["Amt"],
                    "",
                    exec_type,
                    row["TransactTime"],
                    row["SendingTime"],
                    "tick",
                ]
            )
            row_count += 1
    if row_count == 0:
        os.remove(out_path)
        return None
    return channel_seen


def parse_key(line):
    # line is CSV with ChannelNo and ApplSeqNum as the first two fields.
    parts = line.split(",", 2)
    if len(parts) < 2:
        raise ValueError(f"Invalid line: {line!r}")
    return int(parts[0]), int(parts[1])


def merge_batch(input_paths, output_path):
    files = []
    heap = []
    header = None
    try:
        for idx, path in enumerate(input_paths):
            f = open(path, "r", newline="")
            files.append(f)
            file_header = f.readline()
            if header is None:
                header = file_header
            elif file_header != header:
                raise RuntimeError(f"Header mismatch in {path}")
            line = f.readline()
            if line:
                key = parse_key(line)
                heappush(heap, (key, idx, line))
        if header is None:
            return
        with open(output_path, "w", newline="") as out_f:
            out_f.write(header)
            while heap:
                _, idx, line = heappop(heap)
                out_f.write(line)
                next_line = files[idx].readline()
                if next_line:
                    key = parse_key(next_line)
                    heappush(heap, (key, idx, next_line))
    finally:
        for f in files:
            f.close()


def multi_pass_merge(input_paths, output_path, max_open):
    round_id = 0
    paths = list(input_paths)
    while len(paths) > 1:
        next_paths = []
        for i in range(0, len(paths), max_open):
            batch = paths[i : i + max_open]
            if len(batch) == 1:
                next_paths.append(batch[0])
                continue
            merged_path = f"{output_path}.merge_{round_id}_{i // max_open}.tmp"
            merge_batch(batch, merged_path)
            next_paths.append(merged_path)
        paths = next_paths
        round_id += 1
    if paths:
        shutil.move(paths[0], output_path)


def build_event_files(
    archive_path,
    kind,
    out_dir,
    limit_files=None,
    symbol_regex=None,
    only_channel=None,
):
    if kind not in ("order", "tick"):
        raise ValueError("kind must be order or tick")
    pattern = re.compile(symbol_regex) if symbol_regex else None
    event_paths = []
    count = 0
    for inner_path in list_csv_paths(archive_path):
        symbol = os.path.splitext(os.path.basename(inner_path))[0]
        if pattern and not pattern.search(symbol):
            continue
        out_path = os.path.join(out_dir, f"{kind}_{symbol}.events.csv")
        if kind == "order":
            channel = write_order_events(
                archive_path, inner_path, out_path, only_channel=only_channel
            )
        else:
            channel = write_tick_events(
                archive_path, inner_path, out_path, only_channel=only_channel
            )
        if channel is None:
            continue
        event_paths.append((channel, out_path))
        count += 1
        if limit_files is not None and count >= limit_files:
            break
    return event_paths


def main():
    parser = argparse.ArgumentParser(
        description="K-way merge SZSE order+tick CSV streams into a channel-ordered event log."
    )
    parser.add_argument("--order-archive", required=True, help="Path to order_new_STK_SZ_YYYYMMDD.7z")
    parser.add_argument("--tick-archive", required=True, help="Path to tick_new_STK_SZ_YYYYMMDD.7z")
    parser.add_argument("--out", default=None, help="Output CSV path for merged events")
    parser.add_argument("--out-dir", default=None, help="Output directory for per-channel files")
    parser.add_argument("--work-dir", default=None, help="Working directory for temp files")
    parser.add_argument("--keep-temp", action="store_true", help="Keep temp files after merge")
    parser.add_argument("--max-open", type=int, default=64, help="Max files to merge per pass")
    parser.add_argument("--limit-files", type=int, default=None, help="Limit files per archive (for testing)")
    parser.add_argument("--symbol-regex", default=None, help="Regex to filter symbol filenames")
    parser.add_argument("--channel", type=int, default=None, help="Only include a specific ChannelNo")
    args = parser.parse_args()

    temp_root = args.work_dir
    cleanup = False
    if temp_root is None:
        temp_root = tempfile.mkdtemp(prefix="szse_merge_")
        cleanup = not args.keep_temp
    else:
        os.makedirs(temp_root, exist_ok=True)

    try:
        order_dir = os.path.join(temp_root, "order_events")
        tick_dir = os.path.join(temp_root, "tick_events")
        os.makedirs(order_dir, exist_ok=True)
        os.makedirs(tick_dir, exist_ok=True)

        order_paths = build_event_files(
            args.order_archive,
            "order",
            order_dir,
            limit_files=args.limit_files,
            symbol_regex=args.symbol_regex,
            only_channel=args.channel,
        )
        tick_paths = build_event_files(
            args.tick_archive,
            "tick",
            tick_dir,
            limit_files=args.limit_files,
            symbol_regex=args.symbol_regex,
            only_channel=args.channel,
        )

        all_paths = order_paths + tick_paths
        if not all_paths:
            raise RuntimeError("No event files produced; check filters or input archives.")

        if args.out_dir and args.out:
            raise RuntimeError("Use either --out or --out-dir, not both.")
        if not args.out_dir and not args.out:
            raise RuntimeError("Specify --out (single file) or --out-dir (per-channel).")

        if args.out_dir:
            os.makedirs(args.out_dir, exist_ok=True)
            per_channel = {}
            for channel, path in all_paths:
                per_channel.setdefault(channel, []).append(path)
            for channel, paths in per_channel.items():
                out_path = os.path.join(args.out_dir, f"channel_{channel}.csv")
                multi_pass_merge(paths, out_path, max_open=max(2, args.max_open))
        else:
            multi_pass_merge([p for _, p in all_paths], args.out, max_open=max(2, args.max_open))
    finally:
        if cleanup:
            shutil.rmtree(temp_root, ignore_errors=True)


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        print(f"error: {exc}", file=sys.stderr)
        sys.exit(1)
