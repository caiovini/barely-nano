#!/usr/bin/env bash
# Script to build u-boot-pico-itx-k3 for SpacemiT K3 Pico-ITX and optionally
# repartition, format, and flash an NVMe / USB drive (e.g. /dev/sda) with the
# 255 MiB FAT32 NANO_NODE boot partition (LBA 2048..524287, strictly below
# BLOOM_HEADER_LBA = 524,288 / WALLET_STORAGE_LBA = 786,432 / STORAGE_BASE_LBA = 1,048,576)
# containing u-boot.itb, u-boot.bin, boot.scr, config.toml, and barely-nano.bin.
#
# Usage:
#   ./build_u-boot.sh                      # Build u-boot-pico-itx-k3 only
#   ./build_u-boot.sh /dev/sda --flash     # Build U-Boot (+ boot.scr/barely-nano.bin if needed),
#                                          # delete all partitions on /dev/sda, create 255 MiB
#                                          # FAT32 Partition 1 (NANO_NODE), and copy boot files.
#   ./build_u-boot.sh /dev/mtd6 --flash    # Build U-Boot and flash u-boot.itb directly to SPI NOR mtd6

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
UBOOT_DIR="${UBOOT_DIR:-${SCRIPT_DIR}/u-boot-pico-itx-k3}"

DEVICE="/dev/sda"
FLASH_AUTO=false
COMPILE_ARGS=()

for arg in "$@"; do
    case "$arg" in
        --flash)
            FLASH_AUTO=true
            ;;
        -h|--help|help)
            cat <<EOF
Usage: $0 [/dev/sdX | /dev/nvmeXn1 | /dev/mtd6] [--flash] [clean|distclean|defconfig|menuconfig|all]

Options:
  /dev/sdX        Target block device (default: /dev/sda) or SPI NOR MTD device (/dev/mtd6)
  --flash         Delete existing partitions on target disk, create a clean 255 MiB FAT32
                  NANO_NODE boot partition (LBA 2048..524287), format it, and copy:
                    - u-boot.itb & u-boot.bin
                    - boot.scr
                    - config.toml
                    - barely-nano.bin
                  (Or if /dev/mtd6 or /dev/mtdblock6 is specified, flash u-boot.itb directly to SPI NOR.)

Examples:
  $0                           # Compile U-Boot only
  $0 /dev/sda --flash          # Build, wipe/repartition /dev/sda, format FAT32 NANO_NODE, and copy files
  $0 /dev/mtd6 --flash         # On K3 board: flash u-boot.itb directly to onboard SPI NOR mtd6
EOF
            exit 0
            ;;
        /dev/sd[a-z][0-9]*|/dev/mmcblk[0-9]*p[0-9]*|/dev/nvme[0-9]*n[0-9]*p[0-9]*)
            echo "Error: '$arg' is a partition, not a whole disk. Pass the raw disk device (e.g. /dev/sda or /dev/nvme0n1)." >&2
            exit 1
            ;;
        /dev/*)
            DEVICE="$arg"
            ;;
        clean|distclean|defconfig|menuconfig|all)
            COMPILE_ARGS+=("$arg")
            ;;
        *)
            echo "Error: Unknown argument '$arg'" >&2
            echo "Usage: $0 [/dev/sdX] [--flash]" >&2
            exit 1
            ;;
    esac
done

if [[ ! -f "${UBOOT_DIR}/Makefile" ]]; then
    echo "Initializing ${UBOOT_DIR} submodule..."
    git -C "${SCRIPT_DIR}" submodule update --init "${UBOOT_DIR}"
fi

if [[ ! -x "${UBOOT_DIR}/compile.sh" ]]; then
    echo "Error: SpacemiT K3 U-Boot build script not found at ${UBOOT_DIR}/compile.sh" >&2
    exit 1
fi

echo "--- Building SpacemiT K3 U-Boot (${UBOOT_DIR}) ---"
"${UBOOT_DIR}/compile.sh" "${COMPILE_ARGS[@]}"

# Ensure boot.scr and barely-nano.bin are built
cd "${SCRIPT_DIR}"
if [[ ! -f "${SCRIPT_DIR}/boot.scr" || "${SCRIPT_DIR}/boot.cmd" -nt "${SCRIPT_DIR}/boot.scr" ]]; then
    echo "--- Building boot.scr ---"
    make boot.scr
fi
if [[ ! -f "${SCRIPT_DIR}/barely-nano.bin" ]]; then
    echo "--- Building barely-nano.bin ---"
    make build
fi

echo ""
echo "--- Build Complete ---"
ls -lh "${UBOOT_DIR}/u-boot.itb" "${UBOOT_DIR}/u-boot.bin" "${SCRIPT_DIR}/boot.scr" "${SCRIPT_DIR}/config.toml" "${SCRIPT_DIR}/barely-nano.bin"

if [[ "${FLASH_AUTO}" != "true" ]]; then
    echo ""
    echo "To wipe, partition, format, and flash ${DEVICE}, run:"
    echo "  $0 ${DEVICE} --flash"
    exit 0
fi

if [[ ! -e "${DEVICE}" ]]; then
    echo "Error: Target device '${DEVICE}' does not exist." >&2
    exit 1
fi

# Case 1: Direct SPI NOR MTD flash on the K3 board (/dev/mtd6 or /dev/mtdblock6)
if [[ "${DEVICE}" == /dev/mtd* ]]; then
    echo "--- Flashing ${UBOOT_DIR}/u-boot.itb to SPI NOR (${DEVICE}) ---"
    if command -v flashcp >/dev/null 2>&1 && [[ "${DEVICE}" =~ ^/dev/mtd[0-9]+$ ]]; then
        sudo flashcp -v "${UBOOT_DIR}/u-boot.itb" "${DEVICE}"
    else
        MTD_BLK="${DEVICE/\/dev\/mtd/\/dev\/mtdblock}"
        MTD_BLK="${MTD_BLK/mtdblockblock/mtdblock}"
        sudo dd if="${UBOOT_DIR}/u-boot.itb" of="${MTD_BLK}" bs=64K conv=fsync
        sync
    fi
    echo "SPI NOR flash to ${DEVICE} complete."
    exit 0
fi

# Safety guard: refuse to wipe the host root or /mnt/data NVMe drives
ROOT_SRC="$(findmnt -n -o SOURCE / || true)"
DATA_SRC="$(findmnt -n -o SOURCE /mnt/data || true)"
DEV_REAL="$(readlink -f "${DEVICE}")"
if [[ "${DEV_REAL}" == "/dev/nvme0n1" || "${DEV_REAL}" == "/dev/nvme1n1" ]] && [[ "${ROOT_SRC}" == /dev/mapper/* || "${DATA_SRC}" == /dev/nvme1n1* ]]; then
    echo "Error: Refusing to repartition host system NVMe drive ${DEV_REAL}!" >&2
    exit 1
fi

echo ""
echo "=== Preparing ${DEVICE} for SpacemiT K3 Pico-ITX ==="

# 1. Unmount any currently mounted partitions of $DEVICE
echo "[1/4] Unmounting any active partitions on ${DEVICE}..."
for part in $(lsblk -ln -o NAME "${DEVICE}" | tail -n +2); do
    p_dev="/dev/${part}"
    if findmnt -rn -S "${p_dev}" >/dev/null 2>&1; then
        echo "  Unmounting ${p_dev}..."
        udisksctl unmount -b "${p_dev}" 2>/dev/null || sudo umount -l "${p_dev}" 2>/dev/null || true
    fi
done

# 2. Wipe old partition table & create a single 255 MiB FAT32 partition (LBA 2048..524287)
#    Strictly below BLOOM_HEADER_LBA (524,288 = 256 MiB), WALLET_STORAGE_LBA (786,432 = 384 MiB),
#    and STORAGE_BASE_LBA (1,048,576 = 512 MiB).
echo "[2/4] Deleting all existing partitions and creating 255 MiB NANO_NODE boot partition (LBA 2048..524287)..."
sudo wipefs -a "${DEVICE}"
sudo sfdisk --wipe always "${DEVICE}" << 'EOF'
label: dos
start=2048, size=522240, type=0c, bootable
EOF

sudo partprobe "${DEVICE}" 2>/dev/null || true
udevadm settle 2>/dev/null || sleep 1

if [[ "${DEVICE}" =~ [0-9]$ ]]; then
    PART1="${DEVICE}p1"
else
    PART1="${DEVICE}1"
fi

# Wait up to 5 seconds for partition node to appear
for _ in {1..10}; do
    [[ -b "${PART1}" ]] && break
    sleep 0.5
done

if [[ ! -b "${PART1}" ]]; then
    echo "Error: Partition device ${PART1} did not appear after sfdisk." >&2
    exit 1
fi

# 3. Format Partition 1 as FAT32 with label NANO_NODE
echo "[3/4] Formatting ${PART1} as FAT32 (label: NANO_NODE)..."
sudo wipefs -a "${PART1}"
sudo mkfs.vfat -F 32 -n NANO_NODE "${PART1}"

# 4. Mount Partition 1, copy U-Boot and barely-nano boot files, and sync
echo "[4/4] Copying u-boot.itb, u-boot.bin, boot.scr, config.toml, and barely-nano.bin to ${PART1}..."
TMP_MNT="$(mktemp -d /tmp/k3_nano_node.XXXXXX)"
cleanup() {
    if mountpoint -q "${TMP_MNT}" 2>/dev/null; then
        sudo umount "${TMP_MNT}" || true
    fi
    rmdir "${TMP_MNT}" 2>/dev/null || true
}
trap cleanup EXIT

sudo mount "${PART1}" "${TMP_MNT}"
sudo cp -v \
    "${UBOOT_DIR}/u-boot.itb" \
    "${UBOOT_DIR}/u-boot.bin" \
    "${SCRIPT_DIR}/boot.scr" \
    "${SCRIPT_DIR}/config.toml" \
    "${SCRIPT_DIR}/barely-nano.bin" \
    "${TMP_MNT}/"
sync
ls -lh "${TMP_MNT}/"
sudo umount "${TMP_MNT}"
trap - EXIT
rmdir "${TMP_MNT}" 2>/dev/null || true

echo ""
echo "=== Flash Complete on ${DEVICE} (${PART1} = 255 MiB FAT32 NANO_NODE) ==="
lsblk -o NAME,SIZE,FSTYPE,LABEL,MOUNTPOINT "${DEVICE}"
