# SPDX-License-Identifier: GPL-2.0-or-later
#
# Top-level Makefile for the motu424 out-of-tree kernel module + tools.
#
#   make            - build the kernel module (and userspace tools)
#   make module     - build only the kernel module (Rust, kernel/; needs a
#                     CONFIG_RUST kernel, the rustc that built it, and bindgen)
#   make tools      - build the userspace tools (Rust, Cargo workspace in tools/)
#   make gui        - build the GUI (tools/cuemix-studio)
#   make load       - insmod the freshly built module
#   make unload     - rmmod the module
#   make install    - install + depmod (needs root)
#   make ctl        - build + run the CueMix-style control app (status)
#   make clean      - remove build artifacts

KVER    ?= $(shell uname -r)
KDIR    ?= /lib/modules/$(KVER)/build
PWD     := $(shell pwd)
MODNAME := motu424

.PHONY: all module tools gui clean load unload install probe ctl

all: module tools

module:
	$(MAKE) -C $(KDIR) M=$(PWD)/kernel modules

tools:
	$(MAKE) -C tools

gui:
	cd tools && cargo build --release -p cuemix-studio

clean:
	-$(MAKE) -C $(KDIR) M=$(PWD)/kernel clean
	-$(MAKE) -C tools clean

install: module
	$(MAKE) -C $(KDIR) M=$(PWD)/kernel modules_install
	depmod -a

load: module
	sudo insmod kernel/$(MODNAME).ko

unload:
	sudo rmmod $(MODNAME)

probe: tools
	./tools/target/release/motu424-probe

ctl: tools
	-./tools/target/release/motu424-ctl
