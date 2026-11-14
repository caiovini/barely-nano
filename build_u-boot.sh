#!/usr/bin/env bash
# Wrapper script to compile U-Boot for SpacemiT K3 Pico-ITX (u-boot-pico-itx-k3 submodule)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
UBOOT_DIR="${UBOOT_DIR:-${SCRIPT_DIR}/u-boot-pico-itx-k3}"

if [[ ! -x "${UBOOT_DIR}/compile.sh" ]]; then
    echo "Error: SpacemiT K3 U-Boot build script not found at ${UBOOT_DIR}/compile.sh" >&2
    exit 1
fi

exec "${UBOOT_DIR}/compile.sh" "$@"
