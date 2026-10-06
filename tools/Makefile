# SPDX-License-Identifier: GPL-2.0-or-later
CC      ?= gcc
CFLAGS  ?= -O2 -Wall -Wextra

# motu424-ctl needs alsa-lib (libasound + headers). Detect it so a machine
# without the ALSA dev package still builds the probe tool.
ALSA_OK := $(shell pkg-config --exists alsa && echo yes)

TOOLS := motu424-probe motu424-bringup
ifeq ($(ALSA_OK),yes)
TOOLS += motu424-ctl
endif

.PHONY: all clean
all: $(TOOLS)
ifneq ($(ALSA_OK),yes)
	@echo "note: alsa-lib dev not found (pkg-config alsa) -> skipping motu424-ctl"
endif

motu424-probe: motu424-probe.c
	$(CC) $(CFLAGS) -o $@ $<

motu424-bringup: motu424-bringup.c
	$(CC) $(CFLAGS) -o $@ $<

motu424-ctl: motu424-ctl.c
	$(CC) $(CFLAGS) -o $@ $< $(shell pkg-config --cflags --libs alsa)

clean:
	rm -f motu424-probe motu424-ctl motu424-bringup
