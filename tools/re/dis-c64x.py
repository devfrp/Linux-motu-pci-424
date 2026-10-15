#!/usr/bin/env python3
"""Disassemble a raw little-endian TMS320C64x image (loaded at address 0) with Capstone.

Default output is one instruction per line in TI listing style: a leading "||"
marks an instruction that executes in parallel with the one above (p-bit of the
previous word set). Words that do not decode are printed as .word.

--fields lists every data-page-relative access (*+B14[n]) instead, grouped by
field, translated to an offset from --ab (the published audio_base; the DSP's
data page is B14 and audio_base = B14 + 0x13c in the PCI-424 program), with the
start address of the enclosing routine. Routine boundaries are the heuristic
"the instruction after a `b b3` return plus its five delay slots".

usage: dis-c64x.py IMAGE [--base ADDR] [--fields [--dp ADDR] [--ab ADDR]]
"""
import argparse, collections, re, struct
import capstone as cs

# Capstone 5's C64x module gets the A/B register-file side of load/store operands
# wrong (it prints e.g. "stw b3, *a15--[2]" for the standard "STW B3, *B15--[2]"
# prologue push).  Re-derive the operands from the raw word per the C64x CPU ISA
# (SPRU732) load/store formats: bit 3 = 1 selects the 15-bit-offset form whose base
# is B14 (y=0) or B15 (y=1); otherwise base/offset registers are on the .D1 (A, y=0)
# or .D2 (B, y=1) side; the data register side is the s bit (bit 2) in both forms.
_MODES = {0: "*-{b}[{o}]", 1: "*+{b}[{o}]", 4: "*-{b}[{r}]", 5: "*+{b}[{r}]",
          8: "*--{b}[{o}]", 9: "*++{b}[{o}]", 10: "*{b}--[{o}]", 11: "*{b}++[{o}]",
          12: "*--{b}[{r}]", 13: "*++{b}[{r}]", 14: "*{b}--[{r}]", 15: "*{b}++[{r}]"}

def ldst_operands(word, mnemonic, cap_ops):
    """Return corrected 'src, mem' / 'mem, dst' text for a load/store word."""
    s = (word >> 2) & 1
    y = (word >> 7) & 1
    sd = (word >> 23) & 31
    side = "b" if s else "a"
    pair = re.search(r"\b[ab]\d+:[ab]\d+", cap_ops)        # 64-bit loads/stores
    data = f"{side}{sd + 1}:{side}{sd}" if pair else f"{side}{sd}"
    if (word >> 3) & 1:
        mem = f"*+{'b15' if y else 'b14'}[0x{(word >> 8) & 0x7fff:x}]"
    else:
        bs = "b" if y else "a"
        base, off, mode = (word >> 18) & 31, (word >> 13) & 31, (word >> 9) & 15
        mem = _MODES.get(mode, "*?{b}[{o}]").format(b=f"{bs}{base}", o=off, r=f"{bs}{off}")
    return f"{mem}, {data}" if mnemonic.lower().startswith("ld") else f"{data}, {mem}"

ap = argparse.ArgumentParser()
ap.add_argument("image")
ap.add_argument("--base", type=lambda s: int(s, 0), default=0)
ap.add_argument("--fields", action="store_true")
ap.add_argument("--dp", type=lambda s: int(s, 0), default=0x6e70, help="data page (B14) value")
ap.add_argument("--ab", type=lambda s: int(s, 0), default=0x6fac, help="published audio_base")
a = ap.parse_args()

data = open(a.image, "rb").read()
md = cs.Cs(cs.CS_ARCH_TMS320C64X, cs.CS_MODE_LITTLE_ENDIAN)
md.skipdata = True

lines = []          # (addr, word, parallel, mnemonic, op_str)
prev_p = 0
for i in md.disasm(data, a.base):
    word = struct.unpack_from("<I", data, i.address - a.base)[0]
    if i.mnemonic == ".byte":
        lines.append((i.address, word, False, ".word", f"0x{word:08x}"))
        prev_p = 0
        continue
    ops = i.op_str
    if i.mnemonic.lower()[:2] in ("ld", "st") and (word & 2):
        ops = ldst_operands(word, i.mnemonic, ops)
    # Capstone omits the predicate: creg (bits 31-29) 1..6 = B0 B1 B2 A1 A2 A0, z (bit 28) = negate.
    creg, z = word >> 29, (word >> 28) & 1
    if 0 < creg < 7 and i.mnemonic not in ("nop", "NOP"):
        ops = f"[{'!' if z else ''}{('B0','B1','B2','A1','A2','A0')[creg - 1]}] " + ops
    lines.append((i.address, word, bool(prev_p), i.mnemonic, ops))
    prev_p = word & 1

if not a.fields:
    for addr, word, par, mn, ops in lines:
        pred, _, rest = ops.partition("] ") if ops.startswith("[") else ("", "", ops)
        pred = pred + "]" if pred else ""
        print(f"{addr:06x}  {word:08x}  {'||' if par else '  '} {pred:6s}{mn:8s} {rest}")
    raise SystemExit

# routine starts: after each "b b3" return and its 5 delay slots (ignore parallel bars)
starts = [a.base]
idx = {addr: n for n, (addr, *_) in enumerate(lines)}
for n, (addr, word, par, mn, ops) in enumerate(lines):
    if mn.lower() == "b" and ops.strip().lower() == "b3":
        # skip to the first instruction of the execute packet after the 5 delay-slot packets
        k, packets = n, 0
        while packets < 6 and k + 1 < len(lines):
            k += 1
            if not lines[k][2]:
                packets += 1
        starts.append(lines[k][0])
starts = sorted(set(starts))

def routine_of(addr):
    lo, hi = 0, len(starts) - 1
    while lo < hi:
        mid = (lo + hi + 1) // 2
        if starts[mid] <= addr: lo = mid
        else: hi = mid - 1
    return starts[lo]

pat = re.compile(r"\*\+?b14\[(0x[0-9a-f]+|\d+)\]", re.I)
fields = collections.defaultdict(list)
for addr, word, par, mn, ops in lines:
    m = pat.search(ops)
    if not m:
        continue
    n = int(m.group(1), 0)
    kind = "ST" if mn.lower().startswith("st") else "LD" if mn.lower().startswith("ld") else mn
    fields[n].append((addr, kind, mn, ops, routine_of(addr)))

print(f"# data page B14 = 0x{a.dp:x}, audio_base = 0x{a.ab:x} (= B14 + 0x{a.ab - a.dp:x})")
print(f"# {len(starts)} routines by the b-b3 heuristic")
for n in sorted(fields):
    byte = n * 4
    ab = byte - (a.ab - a.dp)
    tag = f"audio_base+0x{ab:x}" if ab >= 0 else f"audio_base-0x{-ab:x}"
    uses = fields[n]
    nld = sum(1 for u in uses if u[1] == "LD"); nst = sum(1 for u in uses if u[1] == "ST")
    print(f"\n== B14[{n}] = 0x{a.dp + byte:x} = {tag}   ({nld} LD, {nst} ST, {len(uses) - nld - nst} other)")
    for addr, kind, mn, ops, rt in uses:
        print(f"   {addr:06x}  routine {rt:06x}  {kind:2s}  {mn:8s} {ops}")
