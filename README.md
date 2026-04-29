# Bare-Metal RISC-V OS (hello-riscv)

This project is a bare-metal RISC-V application designed to run on the QEMU `virt` machine, with the ultimate goal of porting network applications (such as a Nano cryptocurrency node) to a custom RISC-V hardware target like the Lichee Pi 4A. 

Currently, it establishes a foundational operating environment, complete with a heap allocator, interrupt handling, and a full TCP/IP networking stack.

## Features Implemented

### 1. Dual Execution Modes
The kernel can be built to run in two distinct privilege modes using Cargo features (`mmode` or `smode`):
- **Machine Mode (M-mode):** Runs directly on the hardware without any underlying firmware, giving full access to all CSRs and hardware like the CLINT (Core Local Interruptor).
- **Supervisor Mode (S-mode):** Runs on top of OpenSBI firmware. This mirrors the boot chain of advanced RISC-V SoCs (like the TH1520), utilizing SBI `ecall`s for console output and timer configuration.

### 2. Memory Management
- **Boot sequence:** Written in custom assembly to set up the stack pointer, zero the BSS section, and jump into the Rust `kernel_main`.
- **Heap Allocator:** Integrates `linked_list_allocator` to provide a global dynamic memory allocator (`ALLOCATOR`), pre-configured with a 4MB memory arena defined in the linker script.

### 3. Exception and Trap Handling
- **Trap Vector:** Custom assembly trap vector (`trap_vector`) saves and restores the full register context upon interrupts and exceptions.
- **Trap Dispatcher:** The Rust-based trap handler evaluates the `xcause` CSR to differentiate between asynchronous interrupts (like timers) and synchronous exceptions (like access faults).
- **Diagnostics:** Exceptions automatically print out the `xcause`, `sepc`/`mepc`, and `stval`/`mtval` registers over UART to ease debugging.

### 4. Timer Interrupts
- Configured to issue a hardware interrupt every 1 second.
- In M-mode, accesses the hardware `CLINT` MMIO registers to set `mtimecmp`.
- In S-mode, issues an `sbi::set_timer` ecall to request the firmware to trigger the next timer interrupt.

### 5. VirtIO Networking (`virtio-drivers`)
- **MMIO Probing:** Iterates through the standard QEMU `virt` machine memory map (`0x10001000` to `0x10008000`) to discover VirtIO devices.
- **Hardware Abstraction Layer (HAL):** Implements the `Hal` trait mapping physical allocations to our global heap allocator.
- **Initialization:** Automatically negotiates features, configures the MTU, and retrieves the hardware MAC address of the VirtIO Network device.

### 6. TCP/IP Stack (`smoltcp`)
- A custom device wrapper integrates `virtio-net` rx/tx queues directly into the `smoltcp` networking stack.
- Configures a static IP address (defaulting to `10.0.2.15` in QEMU's SLIRP network).
- Currently runs an ICMP socket, successfully responding to network interactions within the simulated network.
- The network stack is continuously polled inside the CPU's idle Wait-For-Interrupt (`wfi`) loop.

### 7. Automated Build & Run System
- A comprehensive `Makefile` handles cross-compilation (`riscv64imac-unknown-none-elf`).
- Launches the QEMU emulator with the correct configurations for either M-mode or S-mode.
- Automatically connects the host network to the VM via `-netdev user` and sets up TCP port forwarding (`hostfwd=tcp::8080-:80`).

## Storage Architecture: Bare-Metal Raw Block Database

Traditional filesystems (like FAT32 or EXT4) introduce substantial overhead and cannot easily handle massive 80GB databases natively in a pure `no_std` environment. To address this, `hello-riscv` employs a **Filesystem-less Raw Block Database Architecture**.

### The Stack
1. **Physical Layer (`virtio-blk`):** The driver exposes raw 512-byte or 4096-byte sectors over the NVMe/Block device interface.
2. **Buffer Pool Manager:** A software cache residing in RAM that manages a pool of in-memory blocks (frames). When the higher-level database engine requests a block (e.g., `Node 142`), the Buffer Pool fetches it from the raw NVMe sector, caches it, and hands it to the engine. It tracks "dirty" pages and writes them back to the disk using an LRU (Least Recently Used) eviction policy.
3. **Database Engine (`btree-slab`):** Instead of using dynamic heap pointers, the database engine allocates nodes from a fixed "slab" abstraction. These slab indices map 1-to-1 with physical NVMe sectors. 

By mapping B-Tree slab nodes directly to physical sectors and managing them via a custom Buffer Pool, we achieve extreme, zero-middleman performance capable of running massive key-value ledgers entirely on bare-metal Rust.

## Running the Project

Ensure you have the RISC-V Rust target installed:
```bash
rustup target add riscv64imac-unknown-none-elf
```

To build and run the kernel in **Supervisor Mode** (default):
```bash
make run
```

To build and run the kernel in **Machine Mode**:
```bash
make run-mmode
```

## Structure
- `/main/` - The main executable binary and entry points.
- `/common/` - The core kernel library, housing the trap handler, UART/SBI wrappers, and the networking stack.
- `linker-mmode.ld` / `linker-smode.ld` - Linker scripts mapping the memory layout for the respective privilege modes.
