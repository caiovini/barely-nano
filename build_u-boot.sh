#!/bin/bash
# Script to build and flash modified u-boot-orangepi-k1 for Orange Pi RV2 (SpacemiT K1 / Ky X1)
# Flashes raw SD card sectors:
# - bootinfo_sd.bin    to sector 0    (0x000000, 80 bytes; preserves MBR partition table at byte 446)
# - FSBL.bin (SPL)     to sector 256  (0x020000, 128 KB offset)
# - u-boot-opensbi.itb to sector 1664 (0x0D0000, 832 KB offset; OpenSBI + U-Boot + DTB FIT image)
# ex: ./build_u-boot.sh /dev/sda --flash

set -e

# Configuration
ARCH=riscv
CROSS_COMPILE=riscv64-linux-gnu-
BOARD_CONFIG=x1_defconfig
UBOOT_DIR="u-boot-orangepi-k1"

# Parse arguments
DEVICE="/dev/sda"
FLASH_AUTO=false

for arg in "$@"; do
    if [[ "$arg" == "--flash" ]]; then
        FLASH_AUTO=true
    elif [[ "$arg" =~ ^/dev/(sd[a-z][0-9]+|mmcblk[0-9]+p[0-9]+|nvme[0-9]+n[0-9]+p[0-9]+)$ ]]; then
        echo "Error: '$arg' is a partition, not a whole disk. Use the raw disk device (e.g. /dev/sdb or /dev/mmcblk0)."
        exit 1
    elif [[ "$arg" =~ ^/dev/ ]]; then
        DEVICE="$arg"
    else
        echo "Error: Unknown argument '$arg'"
        echo "Usage: $0 [/dev/sdX] [--flash]"
        exit 1
    fi
done

if [[ ! -f "$UBOOT_DIR/Makefile" ]]; then
    echo "Initializing $UBOOT_DIR submodule..."
    git submodule update --init "$UBOOT_DIR"
fi

echo "--- Building U-Boot ($UBOOT_DIR) ---"
cd "$UBOOT_DIR"

# 1. Clean
echo "Cleaning..."
make ARCH=$ARCH CROSS_COMPILE=$CROSS_COMPILE clean || true

# 2. Initialize Config (includes CONFIG_API=y)
echo "Initializing config: $BOARD_CONFIG..."
make ARCH=$ARCH CROSS_COMPILE=$CROSS_COMPILE $BOARD_CONFIG

# 3. Build SPL, u-boot-nodtb.bin, DTBs, FSBL.bin, and bootinfo_sd.bin
echo "Building with $(nproc) cores..."
make ARCH=$ARCH CROSS_COMPILE=$CROSS_COMPILE -j$(nproc)

# 4. Create Bin Signatures (FSBL and Bootinfo for SD)
echo "Creating bin signatures (FSBL.bin & bootinfo_sd.bin)..."
cp spl/u-boot-spl.bin board/ky/x1/u-boot-spl.bin

python3 tools/build_binary_file.py \
    -c board/ky/x1/configs/fsbl.json \
    -o FSBL.bin

python3 tools/build_binary_file.py \
    -c board/ky/x1/configs/bootinfo_sd.json \
    -o bootinfo_sd.bin

rm -f board/ky/x1/u-boot-spl.bin

# 5. Package OpenSBI (fw_dynamic.bin) + U-Boot (u-boot-nodtb.bin) + DTB (x1_orangepi-rv2.dtb)
echo "Packaging u-boot-opensbi.itb FIT image..."
cp arch/riscv/dts/x1_orangepi-rv2.dtb ./x1_orangepi-rv2.dtb
./tools/mkimage -f uboot-opensbi.its -r u-boot-opensbi.itb

echo "--- Build Complete ---"
ls -lh bootinfo_sd.bin FSBL.bin u-boot-opensbi.itb

echo ""
echo "To flash to SD card ($DEVICE), run the following commands:"
echo "----------------------------------------------------------------"
echo "sudo dd if=$UBOOT_DIR/bootinfo_sd.bin of=$DEVICE bs=512 count=1 conv=notrunc"
echo "sudo dd if=$UBOOT_DIR/FSBL.bin of=$DEVICE bs=512 seek=256 conv=notrunc"
echo "sudo dd if=$UBOOT_DIR/u-boot-opensbi.itb of=$DEVICE bs=512 seek=1664 conv=notrunc"
echo "sync"
echo "----------------------------------------------------------------"

# Optional: Automatic flash if user passed --flash
if [[ "$FLASH_AUTO" == "true" ]]; then
    echo "Flashing to $DEVICE..."
    sudo dd if=bootinfo_sd.bin of="$DEVICE" bs=512 count=1 conv=notrunc
    sudo dd if=FSBL.bin of="$DEVICE" bs=512 seek=256 conv=notrunc
    sudo dd if=u-boot-opensbi.itb of="$DEVICE" bs=512 seek=1664 conv=notrunc
    sync
    echo "Flash complete."
fi
