# vendor/ — proprietary binaries (local only, git-ignored)

Drop the MOTU Windows driver files here for static reverse engineering. These
are copyrighted vendor binaries and are **never committed** (see `.gitignore`);
only the *facts* extracted from them (register offsets, PCI IDs, DMA format)
end up in the source code.

Priority of what to place here:

1. **`*.sys`** — the kernel driver (the important one; holds the register logic).
   From a Windows box with the driver installed:
   `C:\Windows\System32\drivers\` (look for MOTU / MTPCI / motu424).
2. **`*.inf`** — gives exact PCI IDs (`PCI\VEN_1221&DEV_xxxx`).
3. The installer **`*.exe` / `*.msi`** — only if you don't have the extracted
   `.sys`; it will be unpacked with `7z`.
4. **`HDExpress_FullImageRun.bin`** — PCIe container firmware (ARM32 SoC + Xilinx
   Virtex FPGA bitstream) extracted from `PCIFirmware.cab`. Required for PCIe-424
   bring-up; auto-installed to `/lib/firmware` by `./install.sh`.

Legal basis: reverse engineering for interoperability (writing a driver for
hardware you own) is permitted. Do not redistribute the vendor binaries.
