// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * motu424-bringup - replay the vendor driver's bring-up of a MOTU PCI-424
 * (137a:0004) from userspace, phase by phase, and report what the card's
 * firmware publishes.
 *
 * Hardware model (verified against TI SPRU581C / SPRS219J, see
 * .claude/board-id-2026-09-19.md): the card's PCI interface is the PCI port
 * of a TMS320C6412 DSP in PCI boot mode.
 *   BAR2 (I/O, 16 B): +0 HSR, +4 HDCR, +8 DSPP
 *   BAR1 (8 MB):      DSP register space 0x01800000..  ("window A")
 *   BAR0 (4 MB):      DSP memory page selected by DSPP ("window B")
 *
 * Sequence (vendor fn 0x2c150 for DEV_0004, read from the 32-bit MOTUAW.sys):
 *   P1  HDCR <- 1                       WARMRESET
 *   P2  FPGA image out through the DSP's GPIO (GPEN/GPDIR/GPVAL at window-A
 *       0x300000/4/8): start, 37,177 bytes LSB-first, 1000 trailing clocks
 *   P3  DSPP <- 0; zero-fill DSP RAM 0..0x40000; write the 28,272-byte DSP
 *       program at 0 (+1 zero dword, as the vendor does); read it back
 *   P4  [vendor: HSR <- 0 (unmask PINTA#)]  skipped unless --unmask
 *       mailbox 0x3fffc <- 0; HDCR <- 2 (DSPINT releases the CPU);
 *       poll mailbox until non-zero  ->  audio_base
 *   P5  read-only: audio_base+0/4/8/0x14/0x18 and a dump of the block
 * The vendor's later steps (mixer zeroing, rate registers, +0x50, CECTL0)
 * are deliberately NOT performed.
 *
 * SAFETY
 *   - Default is --dry-run: prints every write it would do, performs none.
 *   - --execute asks for "yes" before each phase (unless --yes).
 *   - Every write goes through one function with a hard allow-list:
 *       window A: only 0x300000, 0x300004, 0x300008 (GPIO)
 *       window B: only 0x000000..0x03ffff while DSPP == 0 (DSP internal RAM)
 *       ports:    HSR <- 0 only, HDCR <- 1|2 only, DSPP <- 0 only
 *     The DSP EEPROM controller (window A 0x420000..) is unreachable by
 *     construction.
 *   - Refuses to run if a kernel driver is bound, if PCIBOOT/EEREAD do not
 *     read as expected, or if the firmware files do not match their sha256.
 *   - Reads are unrestricted.
 *
 * Build: make tools   (no dependencies beyond libc)
 * Run:   sudo ./tools/motu424-bringup                 # dry run
 *        sudo ./tools/motu424-bringup --execute [--until N] [--yes] [--unmask]
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

/* ---- minimal SHA-256 (FIPS 180-4), to avoid a libcrypto dependency ---- */
static const uint32_t K256[64] = {
0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2 };
#define ROR(x,n) (((x) >> (n)) | ((x) << (32 - (n))))
static void sha256_block(uint32_t h[8], const uint8_t *p)
{
	uint32_t w[64], a, b, c, d, e, f, g, hh;
	for (int i = 0; i < 16; i++) w[i] = (uint32_t)p[4*i] << 24 | p[4*i+1] << 16 | p[4*i+2] << 8 | p[4*i+3];
	for (int i = 16; i < 64; i++) {
		uint32_t s0 = ROR(w[i-15],7) ^ ROR(w[i-15],18) ^ (w[i-15] >> 3);
		uint32_t s1 = ROR(w[i-2],17) ^ ROR(w[i-2],19) ^ (w[i-2] >> 10);
		w[i] = w[i-16] + s0 + w[i-7] + s1;
	}
	a=h[0]; b=h[1]; c=h[2]; d=h[3]; e=h[4]; f=h[5]; g=h[6]; hh=h[7];
	for (int i = 0; i < 64; i++) {
		uint32_t t1 = hh + (ROR(e,6) ^ ROR(e,11) ^ ROR(e,25)) + ((e & f) ^ (~e & g)) + K256[i] + w[i];
		uint32_t t2 = (ROR(a,2) ^ ROR(a,13) ^ ROR(a,22)) + ((a & b) ^ (a & c) ^ (b & c));
		hh = g; g = f; f = e; e = d + t1; d = c; c = b; b = a; a = t1 + t2;
	}
	h[0]+=a; h[1]+=b; h[2]+=c; h[3]+=d; h[4]+=e; h[5]+=f; h[6]+=g; h[7]+=hh;
}
static void SHA256(const uint8_t *msg, size_t len, unsigned char out[32])
{
	uint32_t h[8] = {0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19};
	size_t i = 0;
	for (; i + 64 <= len; i += 64) sha256_block(h, msg + i);
	uint8_t tail[128] = {0}; size_t r = len - i; memcpy(tail, msg + i, r); tail[r] = 0x80;
	size_t tl = (r < 56) ? 64 : 128; uint64_t bits = (uint64_t)len * 8;
	for (int k = 0; k < 8; k++) tail[tl - 1 - k] = bits >> (8 * k);
	sha256_block(h, tail); if (tl == 128) sha256_block(h, tail + 64);
	for (int k = 0; k < 8; k++) { out[4*k] = h[k] >> 24; out[4*k+1] = h[k] >> 16; out[4*k+2] = h[k] >> 8; out[4*k+3] = h[k]; }
}

#define FW_DIR          "vendor/firmware/"
#define FW_FPGA         FW_DIR "pci424-serial-container.bin"
#define FW_FPGA_LEN     37177
#define FW_FPGA_SHA     "34e62cf5a75f53a41004129ff1fe6dc435a87e14614d79edc6ce3916e18e04e2"
#define FW_PROG         FW_DIR "pci424-program.bin"
#define FW_PROG_LEN     28272
#define FW_PROG_SHA     "3a23bc97e49154e5eba8956681ee8dd0212cd7091e8d677c272ccb1a7faa6e4f"

#define PORT_HSR   0x0
#define PORT_HDCR  0x4
#define PORT_DSPP  0x8
#define HSR_EEREAD   (1u << 4)
#define HSR_CFGERR   (1u << 3)
#define HSR_INTAM    (1u << 2)
#define HSR_INTAVAL  (1u << 1)
#define HDCR_WARMRESET 1u
#define HDCR_DSPINT    2u
#define HDCR_PCIBOOT   4u

#define WA_GPEN   0x300000u     /* card 0x01b00000 */
#define WA_GPDIR  0x300004u
#define WA_GPVAL  0x300008u
#define RAM_LEN   0x40000u      /* [table+0x1c]: C6412 internal L2 RAM */
#define MAILBOX   0x3fffcu      /* [table+0x30] & 0x3fffff, with DSPP = 0 */

static bool dry = true, yes = false, unmask = false;
static int until = 5;
static int port_fd = -1;
static volatile uint32_t *win_a, *win_b;
static uint32_t cur_dspp = 0;
static unsigned long n_writes;

static void die(const char *fmt, ...)
{
	va_list ap; va_start(ap, fmt); vfprintf(stderr, fmt, ap); va_end(ap);
	fputc('\n', stderr); exit(1);
}

static void udelay(unsigned us)
{
	struct timespec ts = { us / 1000000, (us % 1000000) * 1000L };
	nanosleep(&ts, NULL);
}

/* ---------- the only write paths, each with its allow-list ---------- */

static void port_write(unsigned off, uint32_t val)
{
	if (!((off == PORT_HSR && val == 0) ||
	      (off == PORT_HDCR && (val == HDCR_WARMRESET || val == HDCR_DSPINT)) ||
	      (off == PORT_DSPP && val == 0)))
		die("REFUSED port write +0x%x <- 0x%x (not on allow-list)", off, val);
	n_writes++;
	if (off == PORT_DSPP) cur_dspp = val;	/* tracked in dry run too */
	if (dry) return;
	if (pwrite(port_fd, &val, 4, off) != 4)
		die("port write +0x%x failed: %s", off, strerror(errno));
}

static uint32_t port_read(unsigned off)
{
	uint32_t v = 0;
	if (pread(port_fd, &v, 4, off) != 4)
		die("port read +0x%x failed: %s", off, strerror(errno));
	return v;
}

static void wina_write(uint32_t off, uint32_t val)
{
	if (off != WA_GPEN && off != WA_GPDIR && off != WA_GPVAL)
		die("REFUSED window-A write 0x%06x (only GPIO 0x300000/4/8 allowed)", off);
	n_writes++;
	if (dry) return;
	win_a[off / 4] = val;
}

static void winb_write(uint32_t off, uint32_t val)
{
	if (cur_dspp != 0 || off >= RAM_LEN || (off & 3))
		die("REFUSED window-B write 0x%06x with DSPP=%u (only DSP RAM 0..0x3ffff at page 0)", off, cur_dspp);
	n_writes++;
	if (dry) return;
	win_b[off / 4] = val;
}

static uint32_t wina_read(uint32_t off) { return win_a[off / 4]; }
static uint32_t winb_read(uint32_t off) { return win_b[off / 4]; }

/* ---------- helpers ---------- */

static void hexdump_b(uint32_t off, unsigned len)
{
	for (unsigned o = 0; o < len; o += 16) {
		printf("    %08x:", off + o);
		for (unsigned w = 0; w < 16 && o + w < len; w += 4)
			printf(" %08x", winb_read(off + o + w));
		printf("\n");
	}
}

static uint8_t *load_fw(const char *path, size_t want, const char *sha_hex)
{
	FILE *f = fopen(path, "rb");
	if (!f) die("cannot open %s (run: python3 tools/re/extract-firmware.py)", path);
	uint8_t *buf = malloc(want + 4);
	size_t n = fread(buf, 1, want + 1, f);
	fclose(f);
	if (n != want) die("%s: expected %zu bytes, got %zu", path, want, n);
	unsigned char d[32]; char hex[65];
	SHA256(buf, want, d);
	for (int i = 0; i < 32; i++) sprintf(hex + 2 * i, "%02x", d[i]);
	if (strcmp(hex, sha_hex)) die("%s: sha256 mismatch (%s)", path, hex);
	memset(buf + want, 0, 4);
	printf("  %s: %zu bytes, sha256 ok\n", path, want);
	return buf;
}

static bool confirm(const char *what)
{
	if (dry) { printf("  [dry run] %s\n", what); return true; }
	if (yes) return true;
	char line[16];
	printf("  >>> %s. Type yes to proceed: ", what); fflush(stdout);
	if (!fgets(line, sizeof line, stdin)) return false;
	return !strcmp(line, "yes\n");
}

static void show_ports(const char *tag)
{
	uint32_t hsr = port_read(PORT_HSR), hdcr = port_read(PORT_HDCR), dspp = port_read(PORT_DSPP);
	printf("  [%s] HSR=0x%02x (EEREAD=%u CFGERR=%u INTAM=%u INTAVAL=%u) HDCR=0x%02x (PCIBOOT=%u) DSPP=0x%x\n",
	       tag, hsr, !!(hsr & HSR_EEREAD), !!(hsr & HSR_CFGERR), !!(hsr & HSR_INTAM),
	       !!(hsr & HSR_INTAVAL), hdcr, !!(hdcr & HDCR_PCIBOOT), dspp);
}

/* ---------- phases ---------- */

static void phase1_reset(void)
{
	printf("\nP1  WARMRESET: HDCR <- 1, then wait 1 ms (TI: 16 PCI clocks)\n");
	if (!confirm("1 port write")) exit(2);
	port_write(PORT_HDCR, HDCR_WARMRESET);
	if (!dry) { udelay(1000); show_ports("after reset"); }
}

static void phase2_fpga(const uint8_t *img)
{
	printf("\nP2  FPGA image through GPIO: %u bytes x 24 writes + start/finish = %u window-A writes\n",
	       FW_FPGA_LEN, FW_FPGA_LEN * 24 + 4 + 2001);
	printf("    GPEN<-0xe0 GPDIR<-0xe0 GPVAL<-0 (1us) GPVAL<-0x40; per bit: v=0x40|(b?0x20:0): v, v|0x80, v;\n"
	       "    then 1000 x (0xc0,0x40); GPDIR<-0xc0\n");
	if (!dry) printf("  [before] GPEN=0x%x GPDIR=0x%x GPVAL=0x%x\n",
			 wina_read(WA_GPEN), wina_read(WA_GPDIR), wina_read(WA_GPVAL));
	if (!confirm("~894k GPIO writes (vendor fn 0x251e0/0x25170/0x25230)")) exit(2);
	wina_write(WA_GPEN, 0xe0);
	wina_write(WA_GPDIR, 0xe0);
	wina_write(WA_GPVAL, 0x00);
	if (!dry) udelay(1);
	wina_write(WA_GPVAL, 0x40);
	for (unsigned i = 0; i < FW_FPGA_LEN; i++) {
		uint8_t b = img[i];
		for (unsigned bit = 0; bit < 8; bit++) {
			uint32_t v = 0x40 | ((b >> bit) & 1 ? 0x20 : 0);
			wina_write(WA_GPVAL, v);
			wina_write(WA_GPVAL, v | 0x80);
			wina_write(WA_GPVAL, v);
		}
	}
	for (unsigned i = 0; i < 1000; i++) {
		wina_write(WA_GPVAL, 0xc0);
		wina_write(WA_GPVAL, 0x40);
	}
	wina_write(WA_GPDIR, 0xc0);
	if (!dry) printf("  [after]  GPEN=0x%x GPDIR=0x%x GPVAL=0x%x (GP5 now an input; its level is bit 5)\n",
			 wina_read(WA_GPEN), wina_read(WA_GPDIR), wina_read(WA_GPVAL));
}

static void phase3_program(const uint8_t *prog)
{
	unsigned ndw = FW_PROG_LEN / 4 + 1;	/* vendor: (len>>2)+1 dwords from a zeroed bounce buffer; prog[] has 4 zero bytes appended */
	printf("\nP3  DSPP <- 0; zero-fill DSP RAM 0..0x%x (%u writes); write program at 0 (%u dwords); verify by read-back\n",
	       RAM_LEN, RAM_LEN / 4, ndw);
	if (!dry) { printf("  [before] window B head:\n"); hexdump_b(0, 32); }
	if (!confirm("1 port write + 65536 + 7069 window-B writes")) exit(2);
	port_write(PORT_DSPP, 0);
	for (uint32_t off = 0; off < RAM_LEN; off += 4)
		winb_write(off, 0);
	for (unsigned i = 0; i < ndw; i++) {
		uint32_t v; memcpy(&v, prog + 4 * i, 4);
		winb_write(4 * i, v);
	}
	if (dry) return;
	unsigned bad = 0;
	for (unsigned i = 0; i < ndw; i++) {
		uint32_t v; memcpy(&v, prog + 4 * i, 4);
		if (winb_read(4 * i) != v) bad++;
	}
	uint32_t z = 0;
	for (uint32_t off = FW_PROG_LEN + 4; off < RAM_LEN; off += 4)
		if (winb_read(off)) z++;
	printf("  read-back: %u/%u program dwords differ, %u non-zero dwords in the cleared tail, mailbox=0x%08x\n",
	       bad, ndw, z, winb_read(MAILBOX));
	if (bad || z) {
		printf("  !! DSP RAM did not hold what was written. Window B is not decoding; STOP here.\n");
		exit(3);
	}
	printf("  window B now decodes addresses: writes stick. Head:\n"); hexdump_b(0, 32);
}

static uint32_t phase4_release(void)
{
	printf("\nP4  %s; mailbox 0x%x <- 0; HDCR <- 2 (DSPINT: release CPU); poll mailbox up to 3 s\n",
	       unmask ? "HSR <- 0 (unmask PINTA#, as the vendor does)" : "(HSR unmask SKIPPED: no driver to service IRQs; use --unmask to follow the vendor exactly)",
	       MAILBOX);
	if (!confirm(unmask ? "2 port writes + 1 window-B write" : "1 port write + 1 window-B write")) exit(2);
	if (unmask) port_write(PORT_HSR, 0);
	winb_write(MAILBOX, 0);
	port_write(PORT_HDCR, HDCR_DSPINT);
	if (dry) return 0;
	uint32_t v = 0;
	struct timespec t0, t;
	clock_gettime(CLOCK_MONOTONIC, &t0);
	double waited = 0;
	while (!(v = winb_read(MAILBOX))) {
		udelay(5);	/* nanosleep rounds this up to ~60 us; the deadline is wall-clock */
		clock_gettime(CLOCK_MONOTONIC, &t);
		waited = (t.tv_sec - t0.tv_sec) + (t.tv_nsec - t0.tv_nsec) / 1e9;
		if (waited > 3.0) break;
	}
	show_ports("after DSPINT");
	if (!v) {
		printf("  mailbox stayed 0 after 3 s. DSP did not publish. RAM head now:\n"); hexdump_b(0, 32);
		printf("  (the vendor would now WARMRESET and give up; do a full power cycle before retrying;\n"
		       "   if the program itself changed the RAM head above, the CPU did run: try --unmask next)\n");
		return 0;
	}
	printf("  *** audio_base = 0x%08x (published %.3f s after DSPINT)\n", v, waited);
	return v;
}

static void phase5_inspect(uint32_t ab)
{
	printf("\nP5  read-only inspection of the published block\n");
	if (dry) { printf("  [dry run] would read audio_base+0/4/8/0x14/0x18 and dump 0x140 bytes\n"); return; }
	if ((ab & 0xffc00000u) != 0 || (ab & 3)) {
		printf("  audio_base 0x%08x is outside page 0 (or unaligned); not reading it blind (DSPP would have to change).\n", ab);
		return;
	}
	printf("  mix_base = 0x%08x   (+0)\n", winb_read(ab + 0));
	printf("  +0x04    = 0x%08x\n", winb_read(ab + 4));
	printf("  +0x08    = 0x%08x\n", winb_read(ab + 8));
	printf("  +0x14    = 0x%08x\n", winb_read(ab + 0x14));
	printf("  +0x18    = 0x%08x\n", winb_read(ab + 0x18));
	printf("  block dump:\n"); hexdump_b(ab, 0x140);
	uint32_t mb = winb_read(ab);
	if ((mb & 0xffc00000u) == 0 && !(mb & 3)) { printf("  mix block (45 dwords):\n"); hexdump_b(mb, 0xb4); }
	printf("  mailbox again: 0x%08x; RAM head:\n", winb_read(MAILBOX)); hexdump_b(0, 32);
}

int main(int argc, char **argv)
{
	const char *bdf = NULL;
	for (int i = 1; i < argc; i++) {
		if (!strcmp(argv[i], "--execute")) dry = false;
		else if (!strcmp(argv[i], "--dry-run")) dry = true;
		else if (!strcmp(argv[i], "--yes")) yes = true;
		else if (!strcmp(argv[i], "--unmask")) unmask = true;
		else if (!strcmp(argv[i], "--until") && i + 1 < argc) until = atoi(argv[++i]);
		else if (argv[i][0] != '-') bdf = argv[i];
		else die("usage: %s [BDF] [--execute] [--until N] [--yes] [--unmask]", argv[0]);
	}
	char dev[128] = "";
	if (!bdf) {
		FILE *p = popen("grep -l 0x137a /sys/bus/pci/devices/*/vendor 2>/dev/null | head -1", "r");
		char line[256] = "";
		if (p && fgets(line, sizeof line, p)) { *strrchr(line, '/') = 0; snprintf(dev, sizeof dev, "%s", line); }
		if (p) pclose(p);
		if (!*dev) die("no MOTU (137a) device found");
	} else snprintf(dev, sizeof dev, "/sys/bus/pci/devices/%s", bdf);
	printf("device %s  mode: %s%s\n", dev, dry ? "DRY RUN (no writes)" : "EXECUTE", unmask ? " --unmask" : "");

	char path[200];
	snprintf(path, sizeof path, "%s/driver", dev);
	if (access(path, F_OK) == 0) die("a kernel driver is bound to the device; rmmod it first");
	snprintf(path, sizeof path, "%s/device", dev);
	FILE *f = fopen(path, "r"); char id[16] = ""; if (f) { if (!fgets(id, sizeof id, f)) id[0] = 0; fclose(f); }
	if (strncmp(id, "0x0004", 6)) die("device id %s is not 0x0004; this tool knows only the PCI-424 table", id);

	printf("firmware:\n");
	uint8_t *fpga = load_fw(FW_FPGA, FW_FPGA_LEN, FW_FPGA_SHA);
	uint8_t *prog = load_fw(FW_PROG, FW_PROG_LEN, FW_PROG_SHA);

	snprintf(path, sizeof path, "%s/resource2", dev);
	port_fd = open(path, dry ? O_RDONLY : O_RDWR);
	if (port_fd < 0) die("open %s: %s (need root)", path, strerror(errno));
	int prot = dry ? PROT_READ : PROT_READ | PROT_WRITE, fl = dry ? O_RDONLY : O_RDWR;
	struct stat st;
	snprintf(path, sizeof path, "%s/resource1", dev);
	int fa = open(path, fl); if (fa < 0) die("open %s: %s", path, strerror(errno));
	if (fstat(fa, &st) || st.st_size != (8 << 20)) die("%s is not 8 MB: not the expected window A", path);
	win_a = mmap(NULL, st.st_size, prot, MAP_SHARED, fa, 0);
	snprintf(path, sizeof path, "%s/resource0", dev);
	int fb = open(path, fl); if (fb < 0) die("open %s: %s", path, strerror(errno));
	if (fstat(fb, &st) || st.st_size != (4 << 20)) die("%s is not 4 MB: not the expected window B", path);
	win_b = mmap(NULL, st.st_size, prot, MAP_SHARED, fb, 0);
	if (win_a == MAP_FAILED || win_b == MAP_FAILED) die("mmap failed: %s", strerror(errno));

	printf("pre-flight (reads):\n");
	show_ports("now");
	uint32_t hsr = port_read(PORT_HSR), hdcr = port_read(PORT_HDCR);
	cur_dspp = port_read(PORT_DSPP) & 0x3ff;
	if (!(hdcr & HDCR_PCIBOOT) || !(hsr & HSR_EEREAD) || (hsr & HSR_CFGERR))
		die("pre-flight failed: expected PCIBOOT=1 EEREAD=1 CFGERR=0");
	printf("  GPEN=0x%x GPDIR=0x%x GPVAL=0x%x  CECTL0=0x%08x\n",
	       wina_read(WA_GPEN), wina_read(WA_GPDIR), wina_read(WA_GPVAL), wina_read(8));

	uint32_t ab = 0;
	if (until >= 1) phase1_reset();
	if (until >= 2) phase2_fpga(fpga);
	if (until >= 3) phase3_program(prog);
	if (until >= 4) ab = phase4_release();
	if (until >= 5 && (ab || dry)) phase5_inspect(ab);

	printf("\ndone: %lu writes %s through phase %d.\n", n_writes, dry ? "planned" : "performed", until);
	if (!dry) printf("Full power-off before booting Windows; the DSP state is volatile.\n");
	return 0;
}
