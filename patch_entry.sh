#!/bin/bash
sed -i 's/li   t1, 0x21/li   t1, 0x54\n7:  lb   t2, 20(t0)\n    andi t2, t2, 0x20\n    beqz t2, 7b\n    sb   t1, 0(t0)\n\n    li   t1, 0x52\n71: lb   t2, 20(t0)\n    andi t2, t2, 0x20\n    beqz t2, 71b\n    sb   t1, 0(t0)\n\n    li   t1, 0x41\n72: lb   t2, 20(t0)\n    andi t2, t2, 0x20\n    beqz t2, 72b\n    sb   t1, 0(t0)\n\n    li   t1, 0x50/g' main/src/entry.S
