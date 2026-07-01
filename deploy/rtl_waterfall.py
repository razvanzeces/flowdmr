#!/usr/bin/env python3
"""Render an rtl_power CSV sweep as a waterfall PNG (time x frequency heatmap).

rtl_power writes one CSV line per (timestamp, tuner-hop):
    date, time, f_low, f_high, f_step, n_samples, db, db, db, ...
The db values are the power of each bin from f_low (inclusive) stepping by
f_step. A wide range is tiled across several lines sharing a timestamp; we
regroup them into one waterfall row per timestamp.

Usage: rtl_waterfall.py <scan.csv> <out.png> [marker_mhz]
"""
import sys

import numpy as np
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt


def load(csv_path):
    rows = {}       # timestamp -> {freq_hz: db}
    order = []      # timestamps in first-seen order
    with open(csv_path) as f:
        for line in f:
            parts = [p.strip() for p in line.split(",")]
            if len(parts) < 7:
                continue
            ts = parts[0] + " " + parts[1]
            try:
                f_low = float(parts[2])
                f_step = float(parts[4])
                dbs = [float(x) for x in parts[6:]]
            except ValueError:
                continue
            if ts not in rows:
                rows[ts] = {}
                order.append(ts)
            bucket = rows[ts]
            for i, db in enumerate(dbs):
                bucket[f_low + i * f_step] = db
    return order, rows


def main():
    if len(sys.argv) < 3:
        print("usage: rtl_waterfall.py scan.csv out.png [marker_mhz]")
        sys.exit(2)
    csv_path, out_path = sys.argv[1], sys.argv[2]
    marker = float(sys.argv[3]) * 1e6 if len(sys.argv) > 3 else None

    order, rows = load(csv_path)
    if not order:
        print("no data parsed from", csv_path)
        sys.exit(1)

    freqs = sorted({fr for ts in order for fr in rows[ts]})
    fidx = {fr: i for i, fr in enumerate(freqs)}
    grid = np.full((len(order), len(freqs)), np.nan)
    for r, ts in enumerate(order):
        for fr, db in rows[ts].items():
            grid[r, fidx[fr]] = db

    fmin, fmax = freqs[0], freqs[-1]
    fig, ax = plt.subplots(figsize=(12, max(4.0, len(order) * 0.06 + 2)))
    im = ax.imshow(
        grid, aspect="auto", origin="lower",
        extent=[fmin / 1e6, fmax / 1e6, 0, len(order)],
        cmap="viridis", interpolation="nearest",
    )
    ax.set_xlabel("Frequency (MHz)")
    ax.set_ylabel("Time (sweeps, newest on top)")
    ax.set_title("RTL-SDR waterfall  %.4f-%.4f MHz" % (fmin / 1e6, fmax / 1e6))
    fig.colorbar(im, ax=ax, label="Power (dB)")
    if marker is not None:
        ax.axvline(marker / 1e6, color="red", lw=1.2, ls="--")
        ax.text(marker / 1e6, len(order), " %.4f" % (marker / 1e6),
                color="red", va="bottom", ha="center", fontsize=8)
    fig.tight_layout()
    fig.savefig(out_path, dpi=110)
    print("wrote", out_path)

    # Text summary: strongest bins by median power (what's actually there).
    med = np.nanmedian(grid, axis=0)
    floor = np.nanmedian(med)
    print("noise floor (median of medians): %.1f dB" % floor)
    top = np.argsort(med)[::-1][:10]
    print("Strongest bins (median dB, delta over floor):")
    for i in sorted(top):
        print("  %.4f MHz : %6.1f dB  (+%.1f)" % (freqs[i] / 1e6, med[i], med[i] - floor))


if __name__ == "__main__":
    main()
