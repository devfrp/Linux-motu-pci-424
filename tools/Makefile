# SPDX-License-Identifier: GPL-2.0-or-later
CARGO   ?= cargo

# The Rust tools build with Cargo into target/release/.
RUST_TOOLS := motu424-probe motu424-portregs motu424-bringup motu424-ringwatch motu424-re

# motu424-ctl links alsa-lib (libasound + headers, found by pkg-config).
# Detect it so a machine without the ALSA dev package still builds the rest.
ALSA_OK := $(shell pkg-config --exists alsa && echo yes)
ifeq ($(ALSA_OK),yes)
RUST_TOOLS += motu424-ctl
endif

.PHONY: all rust clean
all: rust
ifneq ($(ALSA_OK),yes)
	@echo "note: alsa-lib dev not found (pkg-config alsa) -> skipping motu424-ctl"
endif

rust:
	$(CARGO) build --release $(addprefix -p ,$(RUST_TOOLS))

clean:
	-$(CARGO) clean
