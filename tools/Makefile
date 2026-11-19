# SPDX-License-Identifier: GPL-2.0-or-later
CC      ?= gcc
CFLAGS  ?= -O2 -Wall -Wextra
CARGO   ?= cargo

# The Rust tools build with Cargo into target/release/.
RUST_TOOLS := motu424-probe motu424-portregs

# motu424-ctl needs alsa-lib (libasound + headers). Detect it so a machine
# without the ALSA dev package still builds the other tools.
ALSA_OK := $(shell pkg-config --exists alsa && echo yes)

C_TOOLS := motu424-bringup
ifeq ($(ALSA_OK),yes)
C_TOOLS += motu424-ctl
endif

.PHONY: all rust clean
all: rust $(C_TOOLS)
ifneq ($(ALSA_OK),yes)
	@echo "note: alsa-lib dev not found (pkg-config alsa) -> skipping motu424-ctl"
endif

rust:
	$(CARGO) build --release $(addprefix -p ,$(RUST_TOOLS))

motu424-bringup: motu424-bringup.c
	$(CC) $(CFLAGS) -o $@ $<

motu424-ctl: motu424-ctl.c
	$(CC) $(CFLAGS) -o $@ $< $(shell pkg-config --cflags --libs alsa)

clean:
	-$(CARGO) clean
	rm -f motu424-ctl motu424-bringup
