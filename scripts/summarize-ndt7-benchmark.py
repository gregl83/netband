#!/usr/bin/env python3
"""Summarize paired reference-client and Netband NDT7 measurements."""

from __future__ import annotations

import csv
import json
import math
import statistics
import sys
from collections import Counter
from pathlib import Path


CLIENTS = ("reference", "netband")
DIRECTIONS = ("download_mbps", "upload_mbps")


def usage() -> None:
    print(f"usage: {Path(sys.argv[0]).name} MEASUREMENTS.csv [OUTPUT_DIR]", file=sys.stderr)


def number(value: str) -> float | None:
    try:
        result = float(value)
    except (TypeError, ValueError):
        return None
    return result if math.isfinite(result) and result >= 0 else None


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    if len(ordered) == 1:
        return ordered[0]
    position = (len(ordered) - 1) * fraction
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return ordered[lower]
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower)


def distribution(values: list[float]) -> dict[str, float | int | None]:
    mean = statistics.fmean(values) if values else None
    sample_sd = statistics.stdev(values) if len(values) > 1 else None
    return {
        "n": len(values),
        "median": statistics.median(values) if values else None,
        "p10": percentile(values, 0.10) if values else None,
        "p90": percentile(values, 0.90) if values else None,
        "mean": mean,
        "sample_sd": sample_sd,
        "cv_pct": (100 * sample_sd / mean) if sample_sd is not None and mean else None,
    }


def fmt(value: float | int | None, digits: int = 2) -> str:
    if value is None:
        return "n/a"
    if isinstance(value, int):
        return str(value)
    return f"{value:.{digits}f}"


def percent(value: float | int | None, digits: int = 2) -> str:
    return "n/a" if value is None else f"{value:.{digits}f}%"


def diagnostic(row: dict[str, str], input_path: Path) -> str:
    recorded = row.get("diagnostic", "")
    if recorded or row.get("client") != "netband":
        return recorded
    raw_file = row.get("raw_file", "")
    if not raw_file:
        return ""
    raw_path = input_path.parent / raw_file
    try:
        with raw_path.open(newline="", encoding="utf-8") as stream:
            events = list(csv.DictReader(stream))
    except (FileNotFoundError, OSError):
        return ""
    failures = []
    for event in events:
        if event.get("event_kind") != "request_failure":
            continue
        kind = event.get("error_kind", "request_failure")
        code = event.get("os_error_code", "")
        failures.append(f"{kind}:{code}" if code else kind)
    return ";".join(failures)


def main() -> int:
    if len(sys.argv) not in (2, 3):
        usage()
        return 2

    input_path = Path(sys.argv[1])
    output_dir = Path(sys.argv[2]) if len(sys.argv) == 3 else input_path.parent
    output_dir.mkdir(parents=True, exist_ok=True)

    with input_path.open(newline="", encoding="utf-8") as stream:
        rows = list(csv.DictReader(stream))

    summary: dict[str, object] = {
        "source": input_path.name,
        "rows": len(rows),
        "clients": {},
        "agreement": {},
    }

    for client in CLIENTS:
        client_rows = [row for row in rows if row["client"] == client]
        successful = [
            row
            for row in client_rows
            if row["exit_code"] == "0"
            and row["outcome"] == "success"
            and all(number(row[field]) is not None for field in DIRECTIONS)
        ]
        client_summary: dict[str, object] = {
            "runs": len(client_rows),
            "successful_runs": len(successful),
            "success_pct": 100 * len(successful) / len(client_rows) if client_rows else None,
            "outcomes": dict(sorted(Counter(row["outcome"] for row in client_rows).items())),
            "diagnostic_runs": sum(bool(diagnostic(row, input_path)) for row in client_rows),
            "diagnostics": dict(
                sorted(
                    Counter(
                        value
                        for row in client_rows
                        if (value := diagnostic(row, input_path))
                    ).items()
                )
            ),
        }
        for direction in DIRECTIONS:
            values = [number(row[direction]) for row in successful]
            client_summary[direction] = distribution([value for value in values if value is not None])
        summary["clients"][client] = client_summary

    by_pair: dict[str, dict[str, dict[str, str]]] = {}
    for row in rows:
        by_pair.setdefault(row["pair"], {})[row["client"]] = row

    for direction in DIRECTIONS:
        signed_differences = []
        absolute_differences = []
        for pair in by_pair.values():
            if not all(client in pair for client in CLIENTS):
                continue
            if any(pair[client]["outcome"] != "success" for client in CLIENTS):
                continue
            reference = number(pair["reference"][direction])
            netband = number(pair["netband"][direction])
            if reference is None or netband is None or reference == 0:
                continue
            signed = 100 * (netband - reference) / reference
            signed_differences.append(signed)
            absolute_differences.append(abs(signed))
        summary["agreement"][direction] = {
            "paired_n": len(signed_differences),
            "median_netband_minus_reference_pct": (
                statistics.median(signed_differences) if signed_differences else None
            ),
            "median_absolute_difference_pct": (
                statistics.median(absolute_differences) if absolute_differences else None
            ),
            "p90_absolute_difference_pct": (
                percentile(absolute_differences, 0.90) if absolute_differences else None
            ),
        }

    json_path = output_dir / "summary.json"
    json_path.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")

    markdown = [
        "# NDT7 comparison summary",
        "",
        "Only runs with exit code 0, outcome `success`, and both throughput values are included",
        "in the distribution statistics. CV is the sample standard deviation divided by the mean.",
        "",
        "| Client | Complete runs | Auxiliary diagnostics | Download median (p10–p90) | Download CV | Upload median (p10–p90) | Upload CV |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for client in CLIENTS:
        stats = summary["clients"][client]
        download = stats["download_mbps"]
        upload = stats["upload_mbps"]
        markdown.append(
            f"| {client} | {stats['successful_runs']}/{stats['runs']} "
            f"({percent(stats['success_pct'], 1)}) | {stats['diagnostic_runs']} | "
            f"{fmt(download['median'])} ({fmt(download['p10'])}–{fmt(download['p90'])}) Mbit/s | "
            f"{percent(download['cv_pct'])} | "
            f"{fmt(upload['median'])} ({fmt(upload['p10'])}–{fmt(upload['p90'])}) Mbit/s | "
            f"{percent(upload['cv_pct'])} |"
        )

    markdown.extend(
        [
            "",
            "| Direction | Paired runs | Median signed difference | Median absolute difference | p90 absolute difference |",
            "| --- | ---: | ---: | ---: | ---: |",
        ]
    )
    for direction in DIRECTIONS:
        stats = summary["agreement"][direction]
        markdown.append(
            f"| {direction.removesuffix('_mbps')} | {stats['paired_n']} | "
            f"{percent(stats['median_netband_minus_reference_pct'])} | "
            f"{percent(stats['median_absolute_difference_pct'])} | "
            f"{percent(stats['p90_absolute_difference_pct'])} |"
        )
    markdown.extend(
        [
            "",
            "Signed difference is `(Netband - reference) / reference × 100` for measurements",
            "with the same pair number.",
            "",
        ]
    )
    markdown_path = output_dir / "summary.md"
    markdown_path.write_text("\n".join(markdown), encoding="utf-8")
    print(markdown_path.read_text(encoding="utf-8"), end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
