#!/usr/bin/env python3
"""Exploratory paired analysis; resample adjacent pairs to preserve order balance."""

import argparse
import csv
import json
import math
import random
import statistics
from pathlib import Path


BOOTSTRAP_SEED = 20260905
BOOTSTRAP_RESAMPLES = 20000


def percentile(values, fraction):
    ordered = sorted(values)
    index = (len(ordered) - 1) * fraction
    lower = int(index)
    return ordered[lower] + (
        ordered[min(lower + 1, len(ordered) - 1)] - ordered[lower]
    ) * (index - lower)


def valid_rate(value):
    try:
        rate = float(value)
    except (TypeError, ValueError):
        return False
    return math.isfinite(rate) and rate > 0


def analyze(rows):
    pairs = {}
    for row in rows:
        if (
            row['outcome'] == 'success'
            and row['exit_code'] == '0'
            and all(valid_rate(row[field]) for field in ('download_mbps', 'upload_mbps'))
        ):
            pair = pairs.setdefault(int(row['pair']), {})
            if row['client'] in pair:
                raise ValueError(f"duplicate client in pair {row['pair']}")
            pair[row['client']] = row
    pairs = {key: pair for key, pair in pairs.items() if set(pair) == {'reference', 'netband'}}
    result = {
        'complete_pairs': len(pairs),
        'bootstrap_seed': BOOTSTRAP_SEED,
        'bootstrap_resamples': BOOTSTRAP_RESAMPLES,
        'method': 'Percentile bootstrap of paired median percentage differences, resampling adjacent two-pair blocks. Exploratory; longer time dependence is not controlled.',
    }
    for direction in ('download', 'upload'):
        field = direction + '_mbps'
        differences = {
            key: 100 * (float(pair['netband'][field]) / float(pair['reference'][field]) - 1)
            for key, pair in pairs.items()
        }
        blocks = [
            [differences[key], differences[key + 1]]
            for key in sorted(pairs)
            if key % 2 and key + 1 in pairs
            and pairs[key]['netband']['position'] == 'first'
            and pairs[key]['reference']['position'] == 'second'
            and pairs[key + 1]['netband']['position'] == 'second'
            and pairs[key + 1]['reference']['position'] == 'first'
        ]
        # Do not drop unmatched pairs or infer uncertainty from a single block.
        randomizer = random.Random(BOOTSTRAP_SEED)
        boots = []
        if len(blocks) >= 2 and len(blocks) * 2 == len(pairs):
            boots = [
                statistics.median([
                    value
                    for block in randomizer.choices(blocks, k=len(blocks))
                    for value in block
                ])
                for _ in range(BOOTSTRAP_RESAMPLES)
            ]
        stats = {
            'paired_median_pct': statistics.median(differences.values()) if differences else None,
            'paired_differences_pct': differences,
            'order': {},
            'block_bootstrap_95_pct': [percentile(boots, .025), percentile(boots, .975)] if boots else None,
            'block_bootstrap_90_pct': [percentile(boots, .05), percentile(boots, .95)] if boots else None,
        }
        for position in ('first', 'second'):
            values = [
                differences[key] for key, pair in pairs.items()
                if pair['netband']['position'] == position
            ]
            stats['order'][position] = {
                'n': len(values),
                'median_pct': statistics.median(values) if values else None,
            }
        interval = stats['block_bootstrap_90_pct']
        stats['within_exploratory_5pct_equivalence_margin'] = bool(
            interval and interval[0] > -5 and interval[1] < 5
        )
        result[direction] = stats
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('measurements', type=Path, metavar='MEASUREMENTS.csv')
    parser.add_argument('output_dir', type=Path, nargs='?', metavar='OUTPUT_DIR')
    args = parser.parse_args()
    with args.measurements.open(newline='', encoding='utf-8') as stream:
        result = analyze(csv.DictReader(stream))
    output_dir = args.output_dir or args.measurements.parent
    output_dir.mkdir(parents=True, exist_ok=True)
    output = json.dumps(result, indent=2) + '\n'
    (output_dir / 'paired-analysis.json').write_text(output, encoding='utf-8')
    print(output, end='')


if __name__ == '__main__':
    main()
