# SPDX-License-Identifier: GPL-2.0-or-later
#
# Top-level Makefile for the motu424 out-of-tree kernel module + tools.
#
#   make            - build the kernel module (and userspace tools)
#   make module     - build only the kernel module
#   make tools      - build the userspace tools (probe + motu424-ctl mixer app)
#   make load       - insmod the freshly built module
#   make unload     - rmmod the module
#   make install    - install + depmod (needs root)
#   make ctl        - build + run the CueMix-style control app (status)
#   make clean      - remove build artifacts

KVER    ?= $(shell uname -r)
KDIR    ?= /lib/modules/$(KVER)/build
PWD     := $(shell pwd)
MODNAME := motu424

.PHONY: all module tools clean load unload install install-firmware probe ctl

all: module tools

module:
	$(MAKE) -C $(KDIR) M=$(PWD)/kernel modules

tools:
	$(MAKE) -C tools

clean:
	-$(MAKE) -C $(KDIR) M=$(PWD)/kernel clean
	-$(MAKE) -C tools clean

install-firmware:
	@if [ -f vendor/HDExpress_FullImageRun.bin ]; then \
		install -d /lib/firmware; \
		install -m 0644 vendor/HDExpress_FullImageRun.bin /lib/firmware/HDExpress_FullImageRun.bin; \
		echo "PCIe firmware installed: /lib/firmware/HDExpress_FullImageRun.bin"; \
	fi

install: module install-firmware
	$(MAKE) -C $(KDIR) M=$(PWD)/kernel modules_install
	depmod -a

load: module
	sudo insmod kernel/$(MODNAME).ko

unload:
	sudo rmmod $(MODNAME)

probe: tools
	./tools/motu424-probe

ctl: tools
	-./tools/motu424-ctl
