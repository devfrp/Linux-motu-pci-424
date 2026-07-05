#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-or-later
#
# install.sh - build & install the motu424 ALSA driver + tools on any distro.
#
# Detects the package manager (pacman/apt/dnf/yum/zypper/apk/xbps), installs the
# build dependencies (kernel headers, gcc/make, alsa-lib dev, dkms), then installs
# the kernel module via DKMS (so it survives kernel upgrades) and the userspace
# tools (motu424-probe, motu424-ctl). Falls back to a plain in-tree build+insmod
# when DKMS is unavailable.
#
#   ./install.sh                 # deps + DKMS install + tools + load
#   ./install.sh -y              # assume "yes" for package installs
#   ./install.sh --no-deps       # skip dependency installation
#   ./install.sh --no-dkms       # plain `make install` instead of DKMS
#   ./install.sh --no-load       # do not modprobe/insmod at the end
#   ./install.sh --tools-only    # just build the userspace tools
#   ./install.sh --uninstall     # remove the module + tools
#   ./install.sh -h              # help
#
# Run from the repository root. Package installs use sudo when not already root.
set -eu

# --------------------------------------------------------------------- options
ASSUME_YES=0; DO_DEPS=1; USE_DKMS=1; DO_LOAD=1; TOOLS_ONLY=0; UNINSTALL=0
NONINTERACTIVE=0
for arg in "$@"; do
	case "$arg" in
	-y|--yes)        ASSUME_YES=1 ;;
	--no-deps)       DO_DEPS=0 ;;
	--no-dkms)       USE_DKMS=0 ;;
	--no-load)       DO_LOAD=0 ;;
	--tools-only)    TOOLS_ONLY=1 ;;
	--uninstall)     UNINSTALL=1 ;;
	-h|--help)       sed -n '4,21p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
	*) echo "unknown option: $arg (try -h)" >&2; exit 2 ;;
	esac
done

# When stdin is not a terminal (piped: `curl ... | sh`), there is no way to
# answer a package-manager prompt, so default to non-interactive installs.
# A real terminal keeps the prompts. (sudo reads /dev/tty for its password
# regardless, so it still works under a pipe.)
if [ ! -t 0 ] && [ "$ASSUME_YES" -eq 0 ]; then
	ASSUME_YES=1
	NONINTERACTIVE=1
fi

# ------------------------------------------------------------------- helpers
SELF_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$SELF_DIR"

log()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m warning:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31m error:\033[0m %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

if [ "$(id -u)" -eq 0 ]; then SUDO=""; else
	if have sudo; then SUDO="sudo"; else
		SUDO=""; warn "not root and no sudo; privileged steps may fail"
	fi
fi

[ -f dkms.conf ] || die "run from the repo root (dkms.conf not found)"
PKG=$(sed -n 's/^PACKAGE_NAME="\(.*\)"/\1/p' dkms.conf)
VER=$(sed -n 's/^PACKAGE_VERSION="\(.*\)"/\1/p' dkms.conf)
: "${PKG:=motu424}" "${VER:=0.1.0}"
KREL=$(uname -r)
KBUILD="/lib/modules/$KREL/build"

# Building any kernel module needs the running kernel's build tree. If the deps
# step was declined or the headers package name was wrong, fail with a clear
# message instead of a cryptic "No such file or directory" from kbuild.
require_headers() {
	[ -d "$KBUILD" ] && return 0
	die "kernel build tree not found: $KBUILD
Install the headers for your running kernel ($KREL) and re-run. Expected package:
  $(kernel_headers_pkg "$PM")
If you just declined the dependency prompt, re-run and accept it (or pass -y)."
}

# --------------------------------------------------------------- distro detect
distro_id() {
	if [ -r /etc/os-release ]; then
		# shellcheck disable=SC1091
		. /etc/os-release
		echo "${ID:-} ${ID_LIKE:-}"
	fi
}

# Echo the kernel-headers package name for the running kernel per pkg manager.
kernel_headers_pkg() {
	case "$1" in
	pacman)
		# Arch: headers pkg is <kernel-flavour>-headers, derived from uname.
		case "$KREL" in
		*-rt*)        echo "linux-rt-headers" ;;
		*-lts*)       echo "linux-lts-headers" ;;
		*-zen*)       echo "linux-zen-headers" ;;
		*-hardened*)  echo "linux-hardened-headers" ;;
		*)            echo "linux-headers" ;;
		esac ;;
	apt)     echo "linux-headers-$KREL" ;;
	dnf|yum) echo "kernel-devel-$KREL" ;;
	zypper)  echo "kernel-devel kernel-default-devel" ;;
	apk)     echo "linux-headers" ;;
	xbps)    echo "linux-headers" ;;
	esac
}

# Detect the package manager and set PM + the dependency package list (DEPS).
detect_pm() {
	if   have pacman; then PM=pacman
	elif have apt-get; then PM=apt
	elif have dnf;    then PM=dnf
	elif have yum;    then PM=yum
	elif have zypper; then PM=zypper
	elif have apk;    then PM=apk
	elif have xbps-install; then PM=xbps
	else PM=unknown; fi

	KH=$(kernel_headers_pkg "$PM")
	case "$PM" in
	pacman) DEPS="base-devel dkms alsa-lib $KH" ;;
	apt)    DEPS="build-essential dkms pkg-config libasound2-dev $KH" ;;
	dnf|yum) DEPS="gcc make dkms pkgconf-pkg-config alsa-lib-devel $KH" ;;
	zypper) DEPS="gcc make dkms pkg-config alsa-devel $KH" ;;
	apk)    DEPS="build-base dkms pkgconf alsa-lib-dev $KH" ;;
	xbps)   DEPS="base-devel dkms pkg-config alsa-lib-devel $KH" ;;
	unknown) DEPS="" ;;
	esac
}

install_deps() {
	[ "$DO_DEPS" -eq 1 ] || { log "skipping dependency install (--no-deps)"; return; }
	if [ "$PM" = unknown ]; then
		warn "unrecognised distro: install these yourself, then rerun with --no-deps:"
		warn "  a C toolchain (gcc/make), kernel headers for $KREL, alsa-lib dev, dkms, pkg-config"
		return
	fi
	[ "$NONINTERACTIVE" -eq 1 ] && \
		log "piped/non-interactive run -> installing packages without prompting"
	log "installing dependencies via $PM: $DEPS"
	y=""; [ "$ASSUME_YES" -eq 1 ] && y="-y"
	case "$PM" in
	pacman) $SUDO pacman -S --needed ${y:+--noconfirm} $DEPS ;;
	apt)    $SUDO apt-get update && $SUDO apt-get install $y $DEPS ;;
	dnf)    $SUDO dnf install $y $DEPS ;;
	yum)    $SUDO yum install $y $DEPS ;;
	zypper) $SUDO zypper install ${y:+-y} $DEPS ;;
	apk)    $SUDO apk add $DEPS ;;
	xbps)   $SUDO xbps-install ${y:+-y} $DEPS ;;
	esac || warn "dependency install reported an error; continuing (deps may already be present)"
}

# --------------------------------------------------------------------- actions
build_tools() {
	log "building userspace tools"
	make tools
	log "installing tools to /usr/local/bin"
	$SUDO install -m0755 tools/motu424-probe /usr/local/bin/ 2>/dev/null || \
		warn "motu424-probe not installed"
	if [ -x tools/motu424-ctl ]; then
		$SUDO install -m0755 tools/motu424-ctl /usr/local/bin/
	else
		warn "motu424-ctl not built (alsa-lib dev missing) - mixer app unavailable"
	fi
}

install_dkms() {
	have dkms || { warn "dkms not found; falling back to in-tree build"; USE_DKMS=0; return; }
	require_headers
	SRC="/usr/src/$PKG-$VER"
	log "installing module via DKMS ($PKG/$VER)"
	$SUDO rm -rf "$SRC"
	$SUDO install -d "$SRC"
	$SUDO cp dkms.conf "$SRC/"
	$SUDO cp -r kernel "$SRC/"
	$SUDO dkms remove "$PKG/$VER" --all >/dev/null 2>&1 || true
	$SUDO dkms add "$PKG/$VER"
	$SUDO dkms build "$PKG/$VER"
	$SUDO dkms install --force "$PKG/$VER"
}

install_intree() {
	require_headers
	log "building + installing module in-tree (no DKMS)"
	make module
	$SUDO make install    # modules_install + depmod
}

load_module() {
	[ "$DO_LOAD" -eq 1 ] || { log "not loading module (--no-load)"; return; }
	$SUDO depmod -a || true
	log "loading module"
	if $SUDO modprobe "$PKG" 2>/dev/null; then :; else
		warn "modprobe failed (module may not be in the modules tree); trying insmod"
		[ -f "kernel/$PKG.ko" ] && $SUDO insmod "kernel/$PKG.ko" || \
			warn "could not load $PKG; check 'dmesg'"
	fi
}

uninstall() {
	log "uninstalling $PKG"
	$SUDO modprobe -r "$PKG" 2>/dev/null || $SUDO rmmod "$PKG" 2>/dev/null || true
	if have dkms; then $SUDO dkms remove "$PKG/$VER" --all 2>/dev/null || true; fi
	$SUDO rm -rf "/usr/src/$PKG-$VER"
	$SUDO rm -f /usr/local/bin/motu424-probe /usr/local/bin/motu424-ctl
	$SUDO depmod -a || true
	log "done. (Module source in this repo was left untouched.)"
}

# ------------------------------------------------------------------------- main
if [ "$UNINSTALL" -eq 1 ]; then uninstall; exit 0; fi

detect_pm
install_deps

if [ "$TOOLS_ONLY" -eq 1 ]; then
	build_tools
	log "tools only: done."
	exit 0
fi

if [ "$USE_DKMS" -eq 1 ]; then install_dkms; fi
if [ "$USE_DKMS" -ne 1 ]; then install_intree; fi
build_tools
load_module

cat <<EOF

$(printf '\033[1;32m==> motu424 installed.\033[0m')

Next steps:
  • Check it came up:   dmesg | tail    (look for the ALSA card registration)
  • List the card:      aplay -l  /  arecord -l
  • Manage the card:    motu424-ctl            (CueMix-style status)
                        motu424-ctl list
  • Remove everything:  ./install.sh --uninstall

Firmware note: the classic PCI-324/424 Altera bitstream (altera424b.rbf) is NOT
shipped here and is not in the 4.0.6 vendor installer. If the driver asks for it
via request_firmware, place it at /lib/firmware/altera424b.rbf. See
docs/fpga-upload.md. (The PCIe HD Express uses a different image.)
EOF
