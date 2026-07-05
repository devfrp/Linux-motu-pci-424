#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-or-later
#
# get.sh - one-line bootstrap installer for the motu424 ALSA driver.
#
#   curl -fsSL https://raw.githubusercontent.com/devfrp/motu-pci-424/main/get.sh | sh
#   curl -fsSL .../get.sh | sh -s -- --no-dkms -y   # pass options to install.sh
#   wget -qO- .../get.sh | sh
#
# Fetches the repository (git clone, or a tarball if git is absent) into a work
# dir and runs ./install.sh, which handles distro detection, dependencies, DKMS
# and the userspace tools. Override with env vars:
#   MOTU424_REPO    git URL         (default: the GitHub repo below)
#   MOTU424_BRANCH  branch/tag      (default: main)
#   MOTU424_DIR     checkout dir    (default: ${TMPDIR:-/tmp}/motu-pci-424)
set -eu

REPO="${MOTU424_REPO:-https://github.com/devfrp/motu-pci-424.git}"
BRANCH="${MOTU424_BRANCH:-main}"
DEST="${MOTU424_DIR:-${TMPDIR:-/tmp}/motu-pci-424}"

# tarball URL derived from a github https repo (used only when git is absent)
SLUG=$(printf '%s' "$REPO" | sed 's,\.git$,,; s,^https://github.com/,,')
TARBALL="https://codeload.github.com/$SLUG/tar.gz/refs/heads/$BRANCH"

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31m error:\033[0m %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

fetch() {
	if have git; then
		log "cloning $REPO ($BRANCH) -> $DEST"
		rm -rf "$DEST"
		git clone --depth 1 --branch "$BRANCH" "$REPO" "$DEST"
	elif have curl || have wget; then
		log "git not found; downloading tarball -> $DEST"
		tmp=$(mktemp -d)
		if have curl; then curl -fsSL "$TARBALL" -o "$tmp/src.tgz"
		else wget -qO "$tmp/src.tgz" "$TARBALL"; fi
		rm -rf "$DEST"; mkdir -p "$DEST"
		tar -xzf "$tmp/src.tgz" -C "$DEST" --strip-components=1
		rm -rf "$tmp"
	else
		die "need 'git', or 'curl'/'wget' to download the sources"
	fi
}

fetch
cd "$DEST" || die "checkout failed"
[ -f install.sh ] || die "install.sh missing in $DEST (wrong branch?)"
log "sources in $DEST - running installer"

# Under `curl ... | sh` stdin is this pipe, not a terminal, so package-manager
# prompts would read EOF and abort. install.sh detects a non-tty stdin and
# switches package installs to non-interactive automatically; sudo still reads
# the terminal directly for its password. (Pass -y to force it anyway.)
exec sh install.sh "$@"
