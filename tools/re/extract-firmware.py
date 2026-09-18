#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-or-later
"""
extract-firmware.py - pull the card firmware images out of YOUR OWN copy of
the vendor driver MOTUAW.sys (MOTU Audio Installer 4.0.6.6814).

The vendor driver carries the FPGA serial image and the card program inside
its .data section and uploads them at every device start. They are MOTU's
copyrighted firmware, so this repository ships only the facts needed to find
them (offsets, lengths, hashes), never the bytes. Output goes to
vendor/firmware/, which is git-ignored.

    python3 tools/re/extract-firmware.py [path/to/MOTUAW.sys ...]

With no argument it tries vendor/x86/MOTUAW.sys and vendor/MOTUAW.sys. Both
the 32-bit and the 64-bit build are recognised by file hash; every image is
verified by sha256 before it is written.
"""
import hashlib
import os
import struct
import sys

BUILDS = {
    "37d68dedde71ff9d9a436f257b5da1eac8b3e031d992c0be5cab17b2d8d8ad0e": "x86",
    "fbbb78cc706caef1d310ec12c503854f307ca4ce6f9c4b8054936d8181607a45": "x64",
}

# name: (x86 file offset, x64 file offset, length, sha256)
IMAGES = {
    "pci424-serial-container.bin": (
        0x257d0, 0x2d8c0, 37177,
        "34e62cf5a75f53a41004129ff1fe6dc435a87e14614d79edc6ce3916e18e04e2"),
    "pci424-bitstream.rbf": (      # data fork of the AppleSingle container
        0x2580e, 0x2d8fe, 36617,
        "e20543f55ea8381e3b3980f396c49c8a3d74f4a26aecaf2226f3d61617cdef9b"),
    "pci424-program.bin": (        # written to card address 0
        0x2e910, 0x36a00, 28272,
        "3a23bc97e49154e5eba8956681ee8dd0212cd7091e8d677c272ccb1a7faa6e4f"),
    "pci324-ep1k30.rbf": (
        0x35840, 0x3d970, 59215,
        "267b343dfc537d114625f8fb8fdea00fc6e1aeb51ea2e6d03fd8b99ee05a5256"),
    "pci324-program.bin": (        # written to card address 0
        0x47610, 0x4f740, 37696,
        "1eae8bad11249f940960be08d1d9d4cb3f5abcda05d614969c137df4892e9c4b"),
    "pci324-data.bin": (           # written to card address 0x80000000
        0x43f90, None, 13952,
        "02b75d8d65e0a19205550270411da0aa90b3828b351daa4bfba6213bee25f264"),
}

OUT_DIR = os.path.join("vendor", "firmware")


def describe_applesingle(blob):
    magic, version = struct.unpack(">II", blob[:8])
    if magic != 0x00051600:
        return
    (count,) = struct.unpack(">H", blob[24:26])
    names = {1: "data fork", 2: "resource fork", 9: "Finder info"}
    print(f"    AppleSingle v{version:#x}, {count} entries:")
    for k in range(count):
        eid, off, ln = struct.unpack(">III", blob[26 + 12 * k:38 + 12 * k])
        print(f"      id {eid} ({names.get(eid, '?')}): offset {off:#x}, length {ln}")


def extract(path):
    raw = open(path, "rb").read()
    build = BUILDS.get(hashlib.sha256(raw).hexdigest())
    if build is None:
        print(f"{path}: not a recognised 4.0.6.6814 MOTUAW.sys build, skipped")
        return False
    print(f"{path}: {build} build recognised")
    os.makedirs(OUT_DIR, exist_ok=True)
    ok = True
    for name, (off86, off64, length, want) in IMAGES.items():
        off = off86 if build == "x86" else off64
        if off is None:
            print(f"  {name}: no offset recorded for the {build} build, skipped")
            continue
        blob = raw[off:off + length]
        got = hashlib.sha256(blob).hexdigest()
        if got != want:
            print(f"  {name}: HASH MISMATCH at {off:#x} (got {got[:16]}...)")
            ok = False
            continue
        with open(os.path.join(OUT_DIR, name), "wb") as f:
            f.write(blob)
        print(f"  {name}: {length} bytes from {off:#x}, sha256 ok")
        if name.endswith("container.bin"):
            describe_applesingle(blob)
    return ok


def main():
    paths = sys.argv[1:] or [p for p in ("vendor/x86/MOTUAW.sys",
                                         "vendor/MOTUAW.sys")
                             if os.path.exists(p)]
    if not paths:
        sys.exit(__doc__)
    results = [extract(p) for p in paths]
    print(f"output directory: {OUT_DIR} (git-ignored)")
    sys.exit(0 if all(results) else 1)


if __name__ == "__main__":
    main()
