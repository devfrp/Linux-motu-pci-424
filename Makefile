# SPDX-License-Identifier: GPL-2.0-or-later
#
# Top-level Makefile for the motu424 out-of-tree kernel module + tools.
#
#   make            - build the kernel module (and userspace tools)
#   make module     - build only the kernel module (C, kernel/)
#   make module-rust - build the Rust kernel module instead (kernel/rust/;
#                     needs CONFIG_RUST and the rustc that built the kernel)
#   make tools      - build the userspace tools (Rust, Cargo workspace in tools/)
#   make gui        - build the GUI (tools/cuemix-studio)
#   make load       - insmod the freshly built module
#   make unload     - rmmod the module
#   make install    - install + depmod (needs root)
#   make install-rust - install the Rust module + depmod (needs root)
#   make load-rust  - insmod the freshly built Rust module
#   make ctl        - build + run the CueMix-style control app (status)
#   make clean      - remove build artifacts

KVER    ?= $(shell uname -r)
KDIR    ?= /lib/modules/$(KVER)/build
PWD     := $(shell pwd)
MODNAME := motu424

.PHONY: all module module-rust tools gui clean load load-rust unload install install-rust probe ctl

all: module tools

module:
	$(MAKE) -C $(KDIR) M=$(PWD)/kernel modules

module-rust:
	$(MAKE) -C $(KDIR) M=$(PWD)/kernel/rust modules

tools:
	$(MAKE) -C tools

gui:
	cd tools && cargo build --release -p cuemix-studio

clean:
	-$(MAKE) -C $(KDIR) M=$(PWD)/kernel clean
	-$(MAKE) -C $(KDIR) M=$(PWD)/kernel/rust clean
	-$(MAKE) -C tools clean

install: module
	$(MAKE) -C $(KDIR) M=$(PWD)/kernel modules_install
	depmod -a

install-rust: module-rust
	$(MAKE) -C $(KDIR) M=$(PWD)/kernel/rust modules_install
	depmod -a

load: module
	sudo insmod kernel/$(MODNAME).ko

load-rust: module-rust
	sudo insmod kernel/rust/$(MODNAME).ko

unload:
	sudo rmmod $(MODNAME)

probe: tools
	./tools/target/release/motu424-probe

ctl: tools
	-./tools/target/release/motu424-ctl
