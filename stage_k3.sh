#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
UBOOT_DIR="${SCRIPT_DIR}/u-boot-pico-itx-k3"
FW_DIR="${UBOOT_DIR}/firmware"

if [[ ! -f "${UBOOT_DIR}/u-boot.bin" ]]; then
    echo "--- Building u-boot-pico-itx-k3 ---"
    "${UBOOT_DIR}/compile.sh"
fi

if [[ ! -f "${SCRIPT_DIR}/boot.scr" || ! -f "${SCRIPT_DIR}/barely-nano.bin" ]]; then
    echo "--- Building boot.scr & barely-nano.bin ---"
    make -C "${SCRIPT_DIR}" build
fi

STAGE_ITB="${UBOOT_DIR}/usb_stage_custom.itb"
TMP_GZ="$(mktemp /tmp/k3_barely_nano.XXXXXX.bin.gz)"
TMP_ITS="$(mktemp /tmp/k3_usb_stage.XXXXXX.its)"
trap 'rm -f "${TMP_GZ}" "${TMP_ITS}"' EXIT

echo "[0/2] Packaging ${STAGE_ITB} from workspace binaries..."
gzip -9 -n -f -c "${SCRIPT_DIR}/barely-nano.bin" > "${TMP_GZ}"

cat > "${TMP_ITS}" <<EOF
/dts-v1/;
/ {
	description = "Custom U-Boot + gzipped barely-nano FIT image for k3 USB stage";
	#address-cells = <2>;
	fit,fdt-list = "of-list";
	images {
		opensbi {
			description = "OpenSBI fw_dynamic Firmware";
			type = "firmware";
			os = "opensbi";
			arch = "riscv";
			compression = "none";
			load = <0x1 0x00000000>;
			entry = <0x1 0x00000000>;
			data = /incbin/("${FW_DIR}/fw_dynamic.bin");
			hash-1 { algo = "crc32"; };
		};
		boot_scr {
			description = "boot.scr";
			type = "standalone";
			arch = "riscv";
			compression = "none";
			load = <0x1 0x1ff00000>;
			data = /incbin/("${SCRIPT_DIR}/boot.scr");
			hash-1 { algo = "crc32"; };
		};
		barely_nano_gz {
			description = "barely-nano.bin.gz";
			type = "standalone";
			arch = "riscv";
			compression = "none";
			load = <0x1 0x20000000>;
			data = /incbin/("${TMP_GZ}");
			hash-1 { algo = "crc32"; };
		};
		uboot {
			description = "U-Boot";
			type = "standalone";
			os = "u-boot";
			arch = "riscv";
			compression = "none";
			load = <0x1 0x02000000>;
			entry = <0x1 0x02000000>;
			data = /incbin/("${UBOOT_DIR}/u-boot.bin");
			hash-1 { algo = "crc32"; };
		};
		fdt_11 {
			description = "k3-pico-itx.dtb";
			type = "flat_dt";
			compression = "none";
			data = /incbin/("${FW_DIR}/opensbi_fdt.dtb");
			hash-1 { algo = "crc32"; };
		};
	};
	configurations {
		default = "conf_11";
		conf_11 {
			description = "k3-pico-itx";
			firmware = "opensbi";
			loadables = "boot_scr", "barely_nano_gz", "uboot";
			fdt = "fdt_11";
		};
	};
};
EOF

mkimage -f "${TMP_ITS}" -E -B 8 "${STAGE_ITB}" >/dev/null
ls -lh "${STAGE_ITB}"

echo "[1/2] Staging FSBL (SPL) to BootROM..."
sudo fastboot stage "${FW_DIR}/fsbl.bin"
sudo fastboot continue || true

echo "Waiting 6.5s for SPL DDR5 training (~5.6s) to enumerate on USB..."
sleep 6.5

echo "[2/2] Staging custom U-Boot + barely-nano.bin.gz to SPL..."
sudo fastboot stage "${STAGE_ITB}"
sudo fastboot continue

echo "Done! Check UART0 console."
