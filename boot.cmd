# Force the Ethernet driver to initialize
ethaddr
ping 10.0.92.1

# Initialize PCIe controller and scan NVMe SSD
nvme scan

# Load config.toml into RAM at 0x10F00000 (64 KB zeroed buffer)
mw.b 0x10F00000 0 0x10000
setenv config_size 0
if fatload mmc 0:1 0x10F00000 config.toml; then
    setenv config_addr 0x10F00000
    setenv config_size ${filesize}
fi

fatload mmc 0:1 0x11000000 barely-nano.bin
go 0x11000000
