#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-or-later
"""
ram-diff.py A.bin B.bin [--start 0x6e70] [--end 0x10000] [--ab 0x6fac]

Compare two 256 KB PCI-424 DSP RAM images dword by dword over [start, end)
and print each differing run with both values, labelled with the audio_base
offset where it falls inside the published block.
"""
import argparse, struct

ap = argparse.ArgumentParser()
ap.add_argument("a"); ap.add_argument("b")
ap.add_argument("--start", type=lambda s: int(s, 0), default=0x6e70)
ap.add_argument("--end", type=lambda s: int(s, 0), default=0x10000)
ap.add_argument("--ab", type=lambda s: int(s, 0), default=0x6fac)
ap.add_argument("--max-run", type=int, default=12, help="dwords shown per run")
o = ap.parse_args()
A = open(o.a, "rb").read(); B = open(o.b, "rb").read()
w = lambda d, x: struct.unpack_from("<I", d, x)[0]

def lab(x):
    off = x - o.ab
    return f"ab+0x{off:03x}" if 0 <= off < 0x200 else f"0x{x:05x}"

run = None
def flush(end):
    global run
    if run is None: return
    n = (end - run) // 4
    xs = range(run, min(end, run + 4 * o.max_run), 4)
    print(f"{lab(run)} .. {lab(end - 4)} ({n} dw)")
    print("   A: " + " ".join(f"{w(A, x):08x}" for x in xs) + (" ..." if n > o.max_run else ""))
    print("   B: " + " ".join(f"{w(B, x):08x}" for x in xs) + (" ..." if n > o.max_run else ""))
    run = None

for x in range(o.start, o.end, 4):
    if w(A, x) != w(B, x):
        if run is None: run = x
    else:
        flush(x)
flush(o.end)
