# Enumerate PCIe buses (Realtek RTL8127 10GbE SFP+ NIC & M.2 NVMe SSD)
pci enum
setenv ethact eth1
nvme scan

# Load config.toml into K3 DRAM at 0x103F00000 (64 KB zeroed buffer, 1 MB below kernel _start)
# Checks USB (usb 0:1) first for easy override, then NVMe Partition 1 (nvme 0:1) or eMMC (mmc 0:1)
mw.b 0x103F00000 0 0x10000
setenv config_size 0
if load usb 0:1 0x103F00000 config.toml || load nvme 0:1 0x103F00000 config.toml || load mmc 0:1 0x103F00000 config.toml; then
    setenv config_addr 0x103F00000
    setenv config_size ${filesize}
fi

if load usb 0:1 0x104000000 barely-nano.bin || load nvme 0:1 0x104000000 barely-nano.bin || load mmc 0:1 0x104000000 barely-nano.bin; then
    go 0x104000000
fi
