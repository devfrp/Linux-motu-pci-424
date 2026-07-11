#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-or-later
"""Disassemble a function starting at an exact VA (no prologue search)."""
import sys
from capstone import Cs, CS_ARCH_X86, CS_MODE_32
import pefile

PATH = "vendor/MOTUAW.sys"

def main():
    if len(sys.argv) < 2:
        sys.exit("usage: disasm-va.py <va> [count]")
    va = int(sys.argv[1], 0)
    count = int(sys.argv[2], 0) if len(sys.argv) > 2 else 200

    pe = pefile.PE(PATH)
    base = pe.OPTIONAL_HEADER.ImageBase
    raw = open(PATH, "rb").read()
    for s in pe.sections:
        name = s.Name.decode('utf-8','replace').rstrip('\x00')
        s_va = s.VirtualAddress + base
        if s_va <= va < s_va + s.Misc_VirtualSize:
            fo = s.PointerToRawData + (va - s_va)
            buf = raw[fo:fo + count*16]
            break
    else:
        sys.exit(f"VA {hex(va)} not in any section")

    md = Cs(CS_ARCH_X86, CS_MODE_32)
    n = 0
    for i in md.disasm(buf, va):
        print(f"  {i.address:#07x}: {i.mnemonic:7s} {i.op_str}")
        n += 1
        if n >= count:
            break
        if i.mnemonic == "ret":
            break

if __name__ == "__main__":
    main()