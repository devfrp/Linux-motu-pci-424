#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-or-later
# Scripted stream test on the PCI-424: load the module, sample the DSP's ring
# position with motu424-ringwatch.py before, during and after a 12 s
# speaker-test, unload, and diff lspci.  Run from a terminal, from the repo
# root, with the firmware images installed under /lib/firmware/motu424/:
#
#     sudo bash tools/stream-run.sh                       # defaults below
#     sudo RUN=run3 ARGS="frame_bytes=96 irq_frames=0x80 period_bytes=0x3000" bash tools/stream-run.sh
#
# This WRITES to the card: module load (bring-up at probe), stream start,
# module unload.  Everything it captures lands in reports/<date>-stream-<RUN>/.
# speaker-test puts its sine on channels 0-3 within 12 s; watch those meters.
set -u
cd "$(dirname "$0")/.."
RUN=${RUN:-run1}
ARGS=${ARGS:-"frame_bytes=96"}
OUT=reports/$(date +%Y-%m-%d)-stream-$RUN
if [ -e "$OUT" ]; then
    echo "$OUT already exists; set RUN=<name> to pick another" >&2
    exit 1
fi
mkdir -p "$OUT"
echo "run $RUN, insmod args: $ARGS" | tee "$OUT/run.log"
# Tell the sampler the period size the driver is using, so its "periods"
# column lines up (driver default is 0x1800 when not given).
PB=$(printf '%s\n' $ARGS | sed -n 's/^period_bytes=//p'); PB=${PB:-0x1800}
WATCHARGS="--period-bytes $PB"
WATCH=
HWP=
log() { echo "== $(date -u +%H:%M:%S) $*" | tee -a "$OUT/run.log"; }

# On any exit (including Ctrl-C or the early bail-out): stop helpers, make
# sure the module is unloaded, and hand the output directory back to the user.
cleanup() {
    [ -n "$WATCH" ] && kill "$WATCH" 2>/dev/null
    [ -n "$HWP" ] && kill "$HWP" 2>/dev/null
    if lsmod | grep -q '^motu424 '; then
        log "cleanup: module still loaded, unloading"
        rmmod motu424 2>&1 | tee -a "$OUT/run.log"
        dmesg -T | tail -10 >> "$OUT/run.log"
    fi
    [ -n "${SUDO_USER:-}" ] && chown -R "$SUDO_USER": "$OUT"
}
trap cleanup EXIT

BDF=$(lspci -d 137a: | head -1 | cut -d" " -f1)
[ -n "$BDF" ] || { log "no MOTU card found"; exit 1; }
log "baseline, card $BDF"
lspci -s "$BDF" -vvv -xxx > "$OUT/lspci-before.txt"
dmesg -T | tail -5 > "$OUT/dmesg-before.txt"
grep -E 'motu424|firewire' /proc/interrupts > "$OUT/irq-before.txt" 2>/dev/null || true

log "load module: $ARGS"
# shellcheck disable=SC2086  # ARGS is a deliberate word list
insmod kernel/motu424.ko $ARGS 2>&1 | tee -a "$OUT/run.log"
sleep 2
dmesg -T | grep -i motu424 | tail -20 | tee "$OUT/dmesg-probe.txt"
aplay -l 2>&1 | tee "$OUT/aplay-l.txt"
cat /proc/asound/cards > "$OUT/asound-cards.txt" 2>/dev/null || true
CARD=$(awk '/motu424/ {gsub(/[^0-9]/,"",$1); print $1; exit}' /proc/asound/cards)
if [ -z "$CARD" ]; then
    log "no ALSA card registered; stopping"
    exit 1
fi
log "ALSA card $CARD"

log "idle sample (2 s, DSP running, no stream)"
python3 tools/motu424-ringwatch.py $WATCHARGS --duration 2 2>&1 | tee "$OUT/ringwatch-idle.txt"

log "stream: speaker-test 24ch 48k S32_LE sine (channels 0-3 get signal in 12 s), 15 s sampler"
python3 tools/motu424-ringwatch.py $WATCHARGS --duration 15 --interval 0.05 \
    > "$OUT/ringwatch-stream.txt" 2>&1 &
WATCH=$!
# Snapshot the negotiated hw_params and the PCM status while the stream is open.
( sleep 6
  cat /proc/asound/card"$CARD"/pcm0p/sub0/hw_params > "$OUT/hw_params.txt" 2>&1
  cat /proc/asound/card"$CARD"/pcm0p/sub0/status > "$OUT/pcm-status.txt" 2>&1
  grep -E 'motu424|firewire' /proc/interrupts > "$OUT/irq-mid.txt" 2>&1 ) &
HWP=$!
sleep 1
timeout 12 speaker-test -D "hw:$CARD" -c 24 -r 48000 -F S32_LE -t sine \
    2>&1 | tee "$OUT/speaker-test.txt"
log "speaker-test ended (124 = stopped by timeout, expected)"
wait $HWP; HWP=
wait $WATCH || log "sampler exited with status $?"
WATCH=
tail -6 "$OUT/ringwatch-stream.txt" | tee -a "$OUT/run.log"
grep -E 'motu424|firewire' /proc/interrupts > "$OUT/irq-after.txt" 2>/dev/null || true
dmesg -T | grep -i motu424 | tail -20 > "$OUT/dmesg-stream.txt"

log "post-stream sample (2 s)"
python3 tools/motu424-ringwatch.py $WATCHARGS --duration 2 2>&1 | tee "$OUT/ringwatch-after.txt"

log "unload module"
rmmod motu424 2>&1 | tee -a "$OUT/run.log"
sleep 1
dmesg -T | tail -10 | tee "$OUT/dmesg-unload.txt"
lspci -s "$BDF" -vvv -xxx > "$OUT/lspci-after.txt"
if diff "$OUT/lspci-before.txt" "$OUT/lspci-after.txt" > "$OUT/lspci-diff.txt"; then
    log "lspci unchanged"
else
    log "lspci DIFFERS, see lspci-diff.txt"
fi
log "done; results in $OUT"
