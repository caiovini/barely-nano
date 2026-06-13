#!/bin/bash
sed -i 's/clean_cache_range(opaque - 32, 64);/clean_cache_range(opaque - 32, 64);\n\n                \/\/ Flush the entire kernel (code, data, bss) to DRAM so Cluster 1 can see it!\n                clean_cache_range(start_addr, 2 * 1024 * 1024);/g' main/src/bin.rs
