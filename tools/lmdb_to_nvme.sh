#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

LDB_PATH="${REPO_ROOT}/database/data.ldb"
NVME_DEV="/dev/sda"
EXTRA_ARGS=()

if [[ $# -ge 1 && "${1}" != -* ]]; then
    LDB_PATH="${1}"
    shift
fi
if [[ $# -ge 1 && "${1}" != -* ]]; then
    NVME_DEV="${1}"
    shift
fi
EXTRA_ARGS=("$@")

DRY_RUN=0
for arg in "${EXTRA_ARGS[@]}"; do
    if [[ "${arg}" == "--dry-run" || "${arg}" == "--help" || "${arg}" == "-h" ]]; then
        DRY_RUN=1
    fi
done

echo "[*] Building lmdb-to-nvme release binary..."
RUSTFLAGS="" cargo build --release --manifest-path "${SCRIPT_DIR}/lmdb-to-nvme/Cargo.toml" --target x86_64-unknown-linux-gnu

BIN="${SCRIPT_DIR}/lmdb-to-nvme/target/x86_64-unknown-linux-gnu/release/lmdb-to-nvme"

if [[ "${DRY_RUN}" -eq 1 || -w "${NVME_DEV}" ]]; then
    exec "${BIN}" --ldb "${LDB_PATH}" --nvme "${NVME_DEV}" "${EXTRA_ARGS[@]}"
else
    echo "[*] Requesting sudo privileges to write directly to ${NVME_DEV}..."
    exec sudo "${BIN}" --ldb "${LDB_PATH}" --nvme "${NVME_DEV}" "${EXTRA_ARGS[@]}"
fi
