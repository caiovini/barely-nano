// SPDX-License-Identifier: GPL-2.0+
/*
 * (C) Copyright 2024
 * Written by: Antigravity AI
 *
 * This file contains routines that fetch data from RISC-V-dependent sources
 * (bd_info etc.) for the U-Boot API.
 */

#include <config.h>
#include <linux/types.h>
#include <api_public.h>

#include <asm/u-boot.h>
#include <asm/global_data.h>

#include "api_private.h"

DECLARE_GLOBAL_DATA_PTR;

int platform_sys_info(struct sys_info *si)
{
	int i;

	for (i = 0; i < CONFIG_NR_DRAM_BANKS; i++)
		platform_set_mr(si, gd->bd->bi_dram[i].start,
				gd->bd->bi_dram[i].size, MR_ATTR_DRAM);

	return 1;
}
