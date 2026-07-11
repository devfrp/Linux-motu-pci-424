#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-or-later
"""
re-devext.py - device-extension type-recovery pass for MOTUAW.sys

Scans .text + INIT for every field access on the device-extension object
(the `this` passed to WriteRegister/ReadRegister/ISR), to extend the partial
field map in docs/vendor-driver-map.md toward a complete struct layout.

Strategy:
  1. Disassemble .text + INIT with capstone (linear sweep, resume past data).
  2. For every [reg+disp] memory operand where disp is in a plausible devext
     range (0x0..0x300), record (insn_addr, mnemonic, base_reg, disp, read/write).
  3. Also record the function each access lives in (nearest preceding prologue).
  4. Output a consolidated field-offset table with all accessor VAs.

This is a superset of xref.py's `mem` command: it scans all sections, classifies
read vs write, and groups by displacement across the whole .text.
"""
import sys
from collections import defaultdict

try:
    from capstone import Cs, CS_ARCH_X86, CS_MODE_32, CS_OP_MEM, CS_OP_IMM, CS_OP_REG
except ImportError:
    sys.exit("capstone missing: pip install capstone")

try:
    import pefile
except ImportError:
    sys.exit("pefile missing: pip install pefile")

PATH = "vendor/MOTUAW.sys"


def load_pe():
    pe = pefile.PE(PATH)
    base = pe.OPTIONAL_HEADER.ImageBase
    sections = []
    for s in pe.sections:
        name = s.Name.decode('utf-8', 'replace').rstrip('\x00')
        va = s.VirtualAddress + base
        fo = s.PointerToRawData
        sz = s.SizeOfRawData
        vsz = s.Misc_VirtualSize
        if sz > 0 and name in ('.text', 'INIT', 'init'):
            sections.append((name, va, fo, min(sz, vsz)))
    return pe, base, sections


def disasm_all(sections):
    """Linear sweep across .text + INIT, resuming past undecodable bytes."""
    md = Cs(CS_ARCH_X86, CS_MODE_32)
    md.detail = True
    raw = open(PATH, "rb").read()
    insns = []
    for name, va, fo, sz in sections:
        buf = raw[fo:fo + sz]
        off = 0
        while off < len(buf):
            got = False
            for i in md.disasm(buf[off:], va + off):
                insns.append(i)
                off = (i.address - va) + i.size
                got = True
            if not got:
                off += 1
    return insns


def find_prologues(insns):
    """Return sorted list of function-start VAs (push ebp prologues)."""
    starts = []
    for i in insns:
        if i.mnemonic == "push" and i.op_str == "ebp":
            starts.append(i.address)
    return starts


def func_of(insn_addr, prologues):
    """Nearest prologue at or before insn_addr."""
    lo, hi = 0, len(prologues)
    while lo < hi:
        mid = (lo + hi) // 2
        if prologues[mid] <= insn_addr:
            lo = mid + 1
        else:
            hi = mid
    if lo == 0:
        return None
    return prologues[lo - 1]


def classify_rw(insn):
    """Crude read/write classification for memory operands."""
    m = insn.mnemonic.lower()
    # Writes: the destination operand is written
    if m in ('mov', 'lea') and len(insn.operands) >= 2:
        # dest is operands[0]
        return 'W' if insn.operands[0].type == CS_OP_MEM else 'R'
    if m in ('add', 'sub', 'and', 'or', 'xor', 'inc', 'dec', 'shl', 'shr', 'sar', 'rol', 'ror'):
        if len(insn.operands) >= 2 and insn.operands[0].type == CS_OP_MEM:
            return 'RW'
        return 'R'
    if m in ('push', 'cmp', 'test'):
        return 'R'
    if m == 'call':
        return 'R'
    if m in ('pop',):
        return 'W'
    # Default: assume read
    return 'R'


def main():
    pe, base, sections = load_pe()
    insns = disasm_all(sections)
    prologues = find_prologues(insns)

    # Collect all [reg+disp] accesses where disp is in 0x0..0x400
    # Focus on ecx/edx/esi/edi (common `this` registers in MSVC thiscall)
    field_accesses = defaultdict(list)  # disp -> list of (addr, func, mnemonic, reg, rw)

    # Also track stack-relative accesses to distinguish struct fields from locals
    # We're interested in accesses where base is a register (not esp/ebp = stack)
    STACK_REGS = {4, 5}  # esp=4, ebp=5 in capstone x86 reg IDs

    for i in insns:
        for op in i.operands:
            if op.type != CS_OP_MEM:
                continue
            if op.mem.base == 0 and op.mem.index == 0:
                continue  # absolute [disp]
            # Skip stack-relative accesses
            if op.mem.base in STACK_REGS:
                continue
            disp = op.mem.disp
            if disp < 0 or disp > 0x400:
                continue
            # This is a [reg+disp] access with disp in struct range
            reg_name = i.reg_name(op.mem.base) if op.mem.base else "none"
            rw = classify_rw(i)
            func = func_of(i.address, prologues)
            field_accesses[disp].append((i.address, func, i.mnemonic, reg_name, rw, i.op_str))

    # Print summary sorted by offset
    print(f"=== Device-extension field access scan (base={hex(base)}) ===")
    print(f"Sections scanned: {', '.join(n for n,_,_,_ in sections)}")
    print(f"Total instructions: {len(insns)}")
    print(f"Functions found: {len(prologues)}")
    print()
    print(f"{'Off':>6s}  {'#acc':>4s}  {'R':>3s} {'W':>3s} {'RW':>3s}  Functions (sample)")
    print("-" * 80)

    for disp in sorted(field_accesses):
        accesses = field_accesses[disp]
        n_r = sum(1 for _,_,_,_,rw,_ in accesses if rw == 'R')
        n_w = sum(1 for _,_,_,_,rw,_ in accesses if rw == 'W')
        n_rw = sum(1 for _,_,_,_,rw,_ in accesses if rw == 'RW')
        funcs = sorted(set(f for _,f,_,_,_,_ in accesses if f is not None))
        func_strs = ' '.join(hex(f) for f in funcs[:8])
        if len(funcs) > 8:
            func_strs += f" (+{len(funcs)-8} more)"
        print(f"{disp:#6x}  {len(accesses):4d}  {n_r:3d} {n_w:3d} {n_rw:3d}  {func_strs}")

    print()

    if len(sys.argv) > 2 and sys.argv[1] == '--detail':
        # Detailed dump for a specific offset
        target = int(sys.argv[2], 0)
        if target in field_accesses:
            print(f"=== Detailed accesses at offset {hex(target)} ===")
            for addr, func, mnem, reg, rw, opstr in sorted(field_accesses[target]):
                print(f"  {addr:#07x} (fn {func:#x if func else 0}): [{rw}] {mnem:7s} {opstr}")
        else:
            print(f"No accesses at offset {hex(target)}")


if __name__ == "__main__":
    main()