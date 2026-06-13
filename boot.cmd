
# Force the Ethernet driver to initialize
ethaddr
ping 10.0.92.1

# Initialize PCIe controller and scan NVMe SSD
nvme scan

fatload mmc 0:1 0x11000000 barely-nano.bin
go 0x11000000
