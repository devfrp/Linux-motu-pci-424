# Clean-room & provenance statement

This document records how the `motu424` driver was produced and on what legal
basis, so that the GPL sources can be redistributed and, eventually, submitted
upstream with a clear provenance trail.

## Legal basis

The MOTU PCI-324 / PCI-424 is undocumented hardware with no vendor-supplied
programming specification. This driver was written to achieve **interoperability**
with hardware the author owns — reading data from and playing audio through the
card under Linux. Reverse engineering strictly for interoperability, where no
documentation is otherwise available, is a recognised and lawful purpose (e.g.
EU Software Directive 2009/24/EC Art. 6; US 17 U.S.C. §1201(f)).

Only **facts** — register offsets, bit meanings, data formats, and access
sequences — were extracted and used. Facts and interface information are not
themselves copyrightable. No vendor code was copied, translated, or adapted into
this repository.

## What was examined, and how

The hardware model was recovered by **static analysis** of the vendor Windows
driver `MOTUAW.sys` (and, for the FPGA question, the shipped installer images).
The methods, all read-only and non-executing:

- disassembly with `objdump` (the binary is never run);
- `tools/re/vtable-scan.py` — resolves C++ vtable slots to function addresses by
  scanning read-only data for pointer runs into `.text`;
- `tools/re/xref.py` — a capstone-based cross-referencer (callers, function
  bodies, immediate/displacement uses, indirect-call sites);
- `strings` for human-readable markers (debug format strings, filenames).

The recovered facts and the exact evidence (virtual addresses, instruction
snippets, confidence tags) are documented in `docs/` —
`register-map.md`, `transport.md`, `fpga-upload.md`, `vendor-driver-map.md` —
so any claim in the driver can be traced back to its origin in the binary.

## What is *not* in this repository

- **No vendor binaries.** `MOTUAW.sys`, `MAWWAVE.sys`, `MotuBus.sys`, the
  installer, and the FPGA images live under `vendor/`, which is **git-ignored**
  and never redistributed. They are inputs to the analysis, not part of the work.
- **No vendor firmware.** The classic PCI-324/424 self-configures its FPGA from
  onboard flash (see `docs/fpga-upload.md`); the driver ships and requires no
  bitstream, so no vendor firmware blob is redistributed.
- **No copied code.** Structure, naming, and comments are original. Where a
  vendor routine's behaviour is described, it is described as a *fact*
  (addresses, sequences), not reproduced as source.

## Boundary between confirmed and unconfirmed

The card is not present on the development machine. The driver deliberately
confines every unverified assumption to `kernel/motu424.h` and
`kernel/motu424_hw.c`, tagged `TODO: verify`, and **refuses to stream** until the
card-reported runtime addresses are supplied (see the README bring-up flow).
This keeps speculative behaviour out of the hardware-agnostic layers and ensures
the driver never writes to addresses that were guessed rather than observed.

## Authorship

Reverse engineering, driver, tooling, and documentation by the repository's
contributors. All facts derive from the author's own analysis of hardware and
binaries the author lawfully possesses.
