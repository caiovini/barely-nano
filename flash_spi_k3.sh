#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
UBOOT_DIR="${SCRIPT_DIR}/u-boot-pico-itx-k3"
FW_DIR="${UBOOT_DIR}/firmware"
ITB="${UBOOT_DIR}/u-boot.itb"

if [[ ! -f "${ITB}" ]]; then
    echo "--- Building ${ITB} via ${UBOOT_DIR}/compile.sh ---"
    "${UBOOT_DIR}/compile.sh"
fi

HEX_LEN=$(printf "0x%x" "$(stat -c '%s' "${ITB}")")

if [[ "${1:-}" != "--direct" ]]; then
    echo "[1/3] Staging FSBL (SPL) to BootROM..."
    sudo fastboot stage "${FW_DIR}/fsbl.bin"
    sudo fastboot continue || true

    echo "Waiting 6.5s for SPL DDR5 training (~5.6s) to enumerate on USB..."
    sleep 6.5

    echo "[2/3] Staging factory U-Boot (for native QSPI SPI NOR driver)..."
    sudo fastboot stage "${FW_DIR}/usb_stage_factory.itb"
    sudo fastboot continue || true

    echo "Waiting 4s for factory U-Boot 'fastboot 0' USB gadget..."
    sleep 4
else
    echo "[1/3] Using active factory U-Boot 'fastboot 0' session (--direct)..."
fi

echo "[2/3] Downloading custom u-boot.itb (${HEX_LEN} bytes) to 0x140000000..."
sudo fastboot stage "${ITB}"

echo "[3/3] Probing SPI NOR, writing @ 0x210000 (mtd6 'uboot'), and verifying readback..."
sudo python3 - "${HEX_LEN}" << 'PYEOF'
import ctypes, fcntl, glob, os, sys

hex_len = sys.argv[1]

USBDEVFS_BULK = 0xC0185502
USBDEVFS_CLAIMINTERFACE = 0x8004550F
USBDEVFS_RELEASEINTERFACE = 0x80045510

class BulkTransfer(ctypes.Structure):
    _fields_ = [
        ("ep", ctypes.c_uint),
        ("len", ctypes.c_uint),
        ("timeout", ctypes.c_uint),
        ("data", ctypes.c_void_p),
    ]

dev_path = None
for d in glob.glob("/sys/bus/usb/devices/*"):
    try:
        if open(f"{d}/idVendor").read().strip() == "361c" and open(f"{d}/idProduct").read().strip() == "1001":
            bus = int(open(f"{d}/busnum").read().strip())
            dev = int(open(f"{d}/devnum").read().strip())
            dev_path = f"/dev/bus/usb/{bus:03d}/{dev:03d}"
            break
    except Exception:
        pass

if not dev_path:
    raise RuntimeError("Could not find USB device 361c:1001")

fd = os.open(dev_path, os.O_RDWR)
iface = ctypes.c_uint(0)
fcntl.ioctl(fd, USBDEVFS_CLAIMINTERFACE, iface)

def fb_cmd(cmd: str, timeout_ms: int = 30000) -> str:
    print(f"  -> {cmd}")
    out_buf = ctypes.create_string_buffer(cmd.encode("ascii"), len(cmd))
    bt_out = BulkTransfer(0x02, len(cmd), timeout_ms, ctypes.cast(out_buf, ctypes.c_void_p))
    fcntl.ioctl(fd, USBDEVFS_BULK, bt_out)

    while True:
        in_buf = ctypes.create_string_buffer(256)
        bt_in = BulkTransfer(0x81, 64, timeout_ms, ctypes.cast(in_buf, ctypes.c_void_p))
        n = fcntl.ioctl(fd, USBDEVFS_BULK, bt_in)
        resp = in_buf.raw[:n].decode("latin1", errors="replace")
        print(f"     <- {resp}")
        if resp.startswith("INFO") or resp.startswith("TEXT"):
            continue
        if not resp.startswith("OKAY"):
            raise RuntimeError(f"Fastboot command failed: {cmd} -> {resp}")
        return resp

try:
    fb_cmd("UCmd:sf probe")
    fb_cmd(f"UCmd:sf update 0x140000000 0x210000 {hex_len}", timeout_ms=60000)
    fb_cmd(f"UCmd:sf read 0x141000000 0x210000 {hex_len}", timeout_ms=30000)
    fb_cmd(f"UCmd:cmp.b 0x140000000 0x141000000 {hex_len}", timeout_ms=30000)
finally:
    try:
        fcntl.ioctl(fd, USBDEVFS_RELEASEINTERFACE, iface)
    except Exception:
        pass
    os.close(fd)
PYEOF

echo "Verified byte-for-byte SPI NOR readback! Rebooting into standalone SPI NOR -> NVMe boot..."
sudo fastboot reboot

echo "=== SPI NOR Flash & Verification Complete! ==="
