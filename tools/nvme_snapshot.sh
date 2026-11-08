#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

echo "[*] Building nvme-snapshot release binary..."
make -C "${REPO_ROOT}" build-nvme-snapshot

BIN="${SCRIPT_DIR}/nvme-snapshot/target/x86_64-unknown-linux-gnu/release/nvme-snapshot"

if [[ $# -eq 0 ]]; then
    exec "${BIN}" --help
fi

MODE="${1}"
for arg in "$@"; do
    if [[ "${arg}" == "--help" || "${arg}" == "-h" ]]; then
        exec "${BIN}" "$@"
    fi
done

# Detect if we need sudo to read or write the block device
NEEDS_SUDO=0
NVME_DEV="/dev/sda"
DRY_RUN=0

ARGS=("$@")
for ((i=0; i<${#ARGS[@]}; i++)); do
    arg="${ARGS[$i]}"
    if [[ "${arg}" == "--dry-run" ]]; then
        DRY_RUN=1
    elif [[ "${arg}" == "--nvme" && $((i+1)) -lt ${#ARGS[@]} ]]; then
        NVME_DEV="${ARGS[$((i+1))]}"
    elif [[ $i -eq 1 && "${arg}" != -* && ( "${MODE}" == "dump" || "${MODE}" == "backup" || "${MODE}" == "read" ) ]]; then
        NVME_DEV="${arg}"
    elif [[ $i -eq 2 && "${arg}" != -* && ( "${MODE}" == "restore" || "${MODE}" == "write" ) ]]; then
        NVME_DEV="${arg}"
    fi
done

if [[ "${MODE}" == "info" || "${MODE}" == "inspect" ]]; then
    NEEDS_SUDO=0
elif [[ "${MODE}" == "dump" || "${MODE}" == "backup" || "${MODE}" == "read" ]]; then
    if [[ ! -r "${NVME_DEV}" ]]; then
        NEEDS_SUDO=1
    fi
elif [[ "${MODE}" == "restore" || "${MODE}" == "write" ]]; then
    if [[ "${DRY_RUN}" -eq 1 ]]; then
        if [[ -e "${NVME_DEV}" && ! -r "${NVME_DEV}" ]]; then
            NEEDS_SUDO=1
        fi
    else
        if [[ ! -w "${NVME_DEV}" ]]; then
            NEEDS_SUDO=1
        fi
    fi
fi

if [[ "${NEEDS_SUDO}" -eq 0 ]]; then
    exec "${BIN}" "$@"
else
    echo "[*] Requesting sudo privileges to access ${NVME_DEV}..."
    exec sudo "${BIN}" "$@"
fi
