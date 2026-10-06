#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-or-later
"""
motu424-portregs.py - READ the PCI-424's three I/O-port registers and decode
them per TI SPRU581C (the card's PCI interface is a TMS320C6412 DSP).

    sudo python3 tools/motu424-portregs.py [0000:02:01.0]

Read-only: it opens the sysfs I/O-port resource O_RDONLY and uses pread().
Per SPRU581C none of these reads has side effects (HSR.INTSRC is cleared only
by writing 1 to it; HDCR's write-only bits read as 0).
"""
import os, sys, glob

bdf = sys.argv[1] if len(sys.argv) > 1 else None
if bdf is None:
    for d in glob.glob("/sys/bus/pci/devices/*"):
        if open(d + "/vendor").read().strip() == "0x137a":
            bdf = os.path.basename(d); break
if bdf is None:
    sys.exit("no MOTU (137a) device found")
dev = f"/sys/bus/pci/devices/{bdf}"
res = open(dev + "/resource").read().splitlines()
io = [i for i, l in enumerate(res) if int(l.split()[2], 16) & 0x100 and int(l.split()[0], 16)]
if not io:
    sys.exit("no I/O-port BAR on this device")
path = f"{dev}/resource{io[0]}"
fd = os.open(path, os.O_RDONLY)
def rd(off):
    return int.from_bytes(os.pread(fd, 4, off), "little")
hsr, hdcr, dspp = rd(0), rd(4), rd(8)
os.close(fd)

print(f"{bdf} I/O BAR {path}")
print(f"  +0x0 HSR  = 0x{hsr:08x}")
print(f"        bit4 EEREAD  = {hsr >> 4 & 1}   (1 = PCI config registers were loaded from the EEPROM)")
print(f"        bit3 CFGERR  = {hsr >> 3 & 1}   (1 = EEPROM autoinit error)")
print(f"        bit2 INTAM   = {hsr >> 2 & 1}   (1 = PINTA# masked)")
print(f"        bit1 INTAVAL = {hsr >> 1 & 1}   (1 = PINTA# currently asserted)")
print(f"        bit0 INTSRC  = {hsr & 1}   (1 = DSP raised an interrupt since last clear)")
print(f"  +0x4 HDCR = 0x{hdcr:08x}")
print(f"        bit2 PCIBOOT = {hdcr >> 2 & 1}   (1 = DSP boots from PCI, CPU stalled until DSPINT)")
print(f"  +0x8 DSPP = 0x{dspp:08x}   (DSP page: window B maps DSP address 0x{(dspp & 0x3ff) << 22:08x})")
print()
if hdcr >> 2 & 1 and hsr >> 4 & 1 and not hsr >> 3 & 1:
    print("VERDICT: matches the model: PCI boot mode, identity from EEPROM, no config error.")
else:
    print("VERDICT: does NOT fully match the expected PCIBOOT=1 / EEREAD=1 / CFGERR=0; stop and review before any write.")
