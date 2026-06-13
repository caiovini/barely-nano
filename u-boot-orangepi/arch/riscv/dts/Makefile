# SPDX-License-Identifier: GPL-2.0+

dtb-$(CONFIG_TARGET_KY_X1) += x1_orangepi-rv2.dtb

include $(srctree)/scripts/Makefile.dts

targets += $(dtb-y)

DTC_FLAGS += -R 4 -p 0x1000

PHONY += dtbs
dtbs: $(addprefix $(obj)/, $(dtb-y))
	@:

clean-files := *.dtb
