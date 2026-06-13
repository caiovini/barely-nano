#!/bin/bash
# Script to build and flash modified u-boot-orangepi for Orange Pi RV2 (JH7110)
# Flashes:
# - u-boot-spl.bin (SPL) to offset 0x0100000 (1MB)
# - u-boot.itb to offset 0x0A00000 (10MB)
# - bootinfo_sd.bin to offset 0x0000400 (1KB) (BootInfo structure for SD)
# ex: sudo ./build_u-boot.sh /dev/sdb --flash


set -e

# Configuration
ARCH=riscv
CROSS_COMPILE=riscv64-linux-gnu-
BOARD_CONFIG=x1_defconfig
UBOOT_DIR="u-boot-orangepi"

# Parse arguments
DEVICE="/dev/sdb"
FLASH_AUTO=false

for arg in "$@"; do
    if [[ "$arg" == "--flash" ]]; then
        FLASH_AUTO=true
    elif [[ "$arg" =~ ^/dev/ ]]; then
        DEVICE="$arg"
    else
        echo "Error: Unknown argument '$arg'"
        echo "Usage: $0 [/dev/sdX] [--flash]"
        exit 1
    fi
done

echo "--- Building U-Boot ---"
cd "$UBOOT_DIR"

# 1. Clean
echo "Cleaning..."
make ARCH=$ARCH CROSS_COMPILE=$CROSS_COMPILE clean

# 2. Initialize Config
echo "Initializing config: $BOARD_CONFIG..."
make ARCH=$ARCH CROSS_COMPILE=$CROSS_COMPILE $BOARD_CONFIG

# 3. Build Binary
echo "Building with $(nproc) cores..."
make ARCH=$ARCH CROSS_COMPILE=$CROSS_COMPILE -j$(nproc)

# 4. Create Bin Signatures (FSBL and Bootinfo)
# Note: The Makefile should trigger these via board/ky/x1/config.mk, 
# but we run them explicitly if they aren't produced or to ensure they are updated.
echo "Creating bin signatures..."

# Ensure we have the SPL in the expected location for the signing tool
cp spl/u-boot-spl.bin board/ky/x1/u-boot-spl.bin

# Sign FSBL
python3 tools/build_binary_file.py \
    -c board/ky/x1/configs/fsbl.json \
    -o FSBL.bin

# Generate Bootinfo for SD
python3 tools/build_binary_file.py \
    -c board/ky/x1/configs/bootinfo_sd.json \
    -o bootinfo_sd.bin

echo "--- Build Complete ---"
ls -l FSBL.bin u-boot.itb bootinfo_sd.bin

echo ""
echo "To flash to SD card ($DEVICE), run the following commands as root:"
echo "----------------------------------------------------------------"
echo "sudo dd if=$UBOOT_DIR/bootinfo_sd.bin of=$DEVICE bs=512 count=1 conv=notrunc"
echo "sudo dd if=$UBOOT_DIR/FSBL.bin of=$DEVICE bs=512 seek=256 conv=notrunc"
echo "sudo dd if=$UBOOT_DIR/u-boot-opensbi.itb of=$DEVICE bs=512 seek=1664 conv=notrunc"
echo "sync"
echo "----------------------------------------------------------------"

# Optional: Automatic sync if user requested
if [[ "$FLASH_AUTO" == "true" ]]; then
    echo "Flashing to $DEVICE..."
    sudo dd if=bootinfo_sd.bin of=$DEVICE bs=512 count=1 conv=notrunc
    sudo dd if=FSBL.bin of=$DEVICE bs=512 seek=256 conv=notrunc
    sudo dd if=u-boot-opensbi.itb of=$DEVICE bs=512 seek=1664 conv=notrunc
    sync
    echo "Flash complete."
fi
