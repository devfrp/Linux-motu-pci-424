#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-or-later
"""
motu424-ringwatch - sample the DSP's published ring position while the
motu424 driver streams.  Reads only: it mmaps the window-B BAR read-only
through sysfs and reads /proc/interrupts.  It can run with the driver bound,
except on kernels built with CONFIG_IO_STRICT_DEVMEM=y (Arch, for one): there
the mmap is refused while the driver holds the BARs, unless the kernel was
booted with iomem=relaxed.

    sudo tools/motu424-ringwatch.py [--bdf 0000:02:01.0] [--audio-base 0x6fac]
                                    [--period-bytes 0x1800] [--interval 0.1]
                                    [--duration 10]

audio_base defaults to the value the driver logged ("DSP running:
audio_base=0x...") and falls back to 0x6fac (bring-up run 1, 2026-10-07).

Columns: elapsed seconds, IRQ count on the motu424 line, +0x2c (position,
host bus address), +0x2c - +0x5c (byte offset into the ring), that offset in
periods, and the raw +0x54/+0x60/+0x64 words so a wrap or a stop is visible.
The last line gives IRQs per second over the whole run.
"""
import argparse, mmap, os, re, subprocess, sys, time

SYS = "/sys/bus/pci/devices"
WINB_SIZE = 4 << 20


def find_card(bdf):
    if bdf:
        return bdf
    for d in sorted(os.listdir(SYS)):
        with open(f"{SYS}/{d}/vendor") as f:
            if f.read().strip() == "0x137a":
                return d
    sys.exit("no MOTU card found")


def winb_resource(bdf):
    with open(f"{SYS}/{bdf}/resource") as f:
        for i, line in enumerate(f):
            start, end, flags = (int(x, 16) for x in line.split())
            if flags & 0x200 and end - start + 1 == WINB_SIZE:
                return f"{SYS}/{bdf}/resource{i}"
    sys.exit("no 4 MB MMIO BAR (window B) on this card")


def audio_base_from_dmesg():
    try:
        out = subprocess.run(["dmesg"], capture_output=True, text=True).stdout
    except OSError:
        return None
    m = re.findall(r"motu424.*audio_base=0x([0-9a-f]+)", out)
    return int(m[-1], 16) if m else None


def irq_count():
    with open("/proc/interrupts") as f:
        for line in f:
            if "motu424" in line:
                return sum(int(x) for x in line.split()[1:] if x.isdigit())
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bdf")
    ap.add_argument("--audio-base", type=lambda s: int(s, 0))
    ap.add_argument("--period-bytes", type=lambda s: int(s, 0), default=0x1800)
    ap.add_argument("--interval", type=float, default=0.1)
    ap.add_argument("--duration", type=float, default=10.0)
    ap.add_argument("--dump", metavar="FILE",
                    help="write the 256 KB DSP RAM image (window B 0..0x3ffff) "
                         "to FILE and exit; reads only")
    a = ap.parse_args()

    bdf = find_card(a.bdf)
    ab = a.audio_base or audio_base_from_dmesg() or 0x6fac
    res = winb_resource(bdf)
    fd = os.open(res, os.O_RDONLY | os.O_SYNC)
    try:
        mm = mmap.mmap(fd, WINB_SIZE, prot=mmap.PROT_READ)
    except OSError as e:
        sys.exit(f"cannot mmap {res}: {e.strerror}; with the driver bound, a "
                 "CONFIG_IO_STRICT_DEVMEM=y kernel needs iomem=relaxed")

    def rd(off):
        return int.from_bytes(mm[ab + off:ab + off + 4], "little")

    if a.dump:
        with open(a.dump, "wb") as f:
            f.write(mm[0:0x40000])
        print(f"# wrote 256 KB of DSP RAM from {res} to {a.dump}")
        return

    print(f"# card {bdf}, window B {res}, audio_base 0x{ab:x}, "
          f"period_bytes 0x{a.period_bytes:x}")
    print(f"# +0x0c=0x{rd(0x0c):x} +0x10=0x{rd(0x10):x} +0x58=0x{rd(0x58):x} "
          f"+0x68=0x{rd(0x68):x} +0x6c=0x{rd(0x6c):x} +0x8c=0x{rd(0x8c):x} "
          f"+0x80=0x{rd(0x80):x} +0x98=0x{rd(0x98):x}")
    print("#     t   irqs      +0x2c      +0x5c    offset  periods  +0x54 +0x60 +0x64   +0x128   +0x12c   +0x130")

    t0 = time.monotonic()
    irq0 = irq_count()
    last = None
    prev_irqs = irq0
    first_tick = last_tick = None   # (t, irqs) at the first/last IRQ change
    while True:
        t = time.monotonic() - t0
        irqs = irq_count()
        pos, base = rd(0x2c), rd(0x5c)
        off = (pos - base) & 0xffffffff
        row = (irqs, pos, base, rd(0x54), rd(0x60), rd(0x64),
               rd(0x128), rd(0x12c), rd(0x130))
        if irqs is not None and prev_irqs is not None and irqs != prev_irqs:
            if first_tick is None:
                first_tick = (t, prev_irqs)
            last_tick = (t, irqs)
        prev_irqs = irqs
        done = t >= a.duration
        if row != last or done:
            per = f"{off / a.period_bytes:7.3f}" if a.period_bytes else "      -"
            print(f"{t:7.3f} {irqs if irqs is not None else -1:6d} "
                  f"0x{pos:08x} 0x{base:08x} 0x{off:07x} {per}  "
                  f"{row[3]:5x} {row[4]:5x} {row[5]:5x} "
                  f"{row[6]:8x} {row[7]:8x} {row[8]:8x}")
            last = row
        if done:
            break
        time.sleep(a.interval)
    irq1 = irq_count()
    if irq0 is not None and irq1 is not None and t > 0:
        print(f"# whole run: {irq1 - irq0} IRQs in {t:.3f} s = "
              f"{(irq1 - irq0) / t:.2f}/s")
    if first_tick and last_tick and last_tick[0] > first_tick[0]:
        n = last_tick[1] - first_tick[1]
        dt = last_tick[0] - first_tick[0]
        print(f"# while ticking: {n} IRQs in {dt:.3f} s = {n / dt:.2f}/s "
              f"(0x80 frames/IRQ: 750/s at 96 kHz, 375/s at 48 kHz; "
              f"the line may be shared, subtract the other device's count)")


if __name__ == "__main__":
    main()
