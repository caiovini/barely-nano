#!/usr/bin/env bash
# Wrapper script to build U-Boot for SpacemiT K3 Pico-ITX (/mnt/data/rust-workspace/u-boot)
set -euo pipefail

UBOOT_DIR="${UBOOT_DIR:-/mnt/data/rust-workspace/u-boot}"

if [[ ! -x "${UBOOT_DIR}/build.sh" ]]; then
    echo "Error: SpacemiT K3 U-Boot build script not found at ${UBOOT_DIR}/build.sh" >&2
    exit 1
fi

exec "${UBOOT_DIR}/build.sh" "$@"
