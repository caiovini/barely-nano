KERNEL_MMODE = target/riscv64gc-unknown-none-elf/release/barely-nano
KERNEL_SMODE = target/riscv64gc-unknown-none-elf/release/barely-nano
CARGO = /home/cvini/.asdf/shims/cargo

.PHONY: build-mmode build-smode build-spammer run-mmode run-smode run-spammer kill clean

disk.img:
	dd if=/dev/zero of=$@ bs=1M count=1024

# ── M-mode ──────────────────────────────────────────────────────────────────
# Runs entirely in machine mode; no firmware needed.
build-mmode:
	$(CARGO) build -p barely-nano --release --target riscv64gc-unknown-none-elf --no-default-features --features mmode

run-mmode: build-mmode disk.img
	qemu-system-riscv64 \
	  -machine virt \
	  -smp 8 \
	  -cpu rv64,v=false,zvkb=false,zbb=false,zicbom=false \
	  -nographic \
	  -bios none \
	  -kernel $(KERNEL_MMODE) \
	  -netdev user,id=n0,hostfwd=tcp::7076-:7076,hostfwd=tcp::7075-:7075 \
	  -device virtio-net-device,netdev=n0 \
	  -drive file=disk.img,if=none,format=raw,id=hd0,cache=writethrough \
	  -device virtio-blk-device,drive=hd0,queue-size=128

# ── S-mode (default) ─────────────────────────────────────────────────────────
# OpenSBI (bundled with QEMU) runs in M-mode; our kernel runs in S-mode.
# This mirrors the exact boot chain used on the orange pi.
build-smode:
	RUSTFLAGS="-C target-feature=+v,+zbb,+zba,+zbs,+zbc,+zihintpause,+zicboz,+zicbom" $(CARGO) build -p barely-nano --release --target riscv64gc-unknown-none-elf --features simd
	llvm-objcopy -O binary target/riscv64gc-unknown-none-elf/release/barely-nano barely-nano.bin
	@if [ -d "/run/media/cvini/NANO_NODE" ]; then \
		cp barely-nano.bin /run/media/cvini/NANO_NODE/; \
		sync; \
		echo "Successfully deployed barely-nano.bin to SD card"; \
	fi

# ── S-mode + local block spammer ─────────────────────────────────────────────
# Same as smode but with the local spammer enabled for B-Tree / throughput testing.
# Use 'make run-spammer' for testing, 'make run' for live network.
build-spammer:
	RUSTFLAGS="-C target-feature=+v,+zbb,+zba,+zbs,+zbc,+zihintpause,+zicboz" $(CARGO) build -p barely-nano --release --target riscv64gc-unknown-none-elf --features spammer,simd
	llvm-objcopy -O binary target/riscv64gc-unknown-none-elf/release/barely-nano barely-nano.bin
	@if [ -d "/run/media/cvini/NANO_NODE" ]; then \
		cp barely-nano.bin /run/media/cvini/NANO_NODE/; \
		sync; \
		echo "Successfully deployed barely-nano.bin to SD card"; \
	fi

run-spammer: build-spammer disk.img
	qemu-system-riscv64 \
	  -machine virt \
	  -accel tcg,thread=multi \
	  -smp 8 \
	  -m 4G \
	  -cpu rv64,v=true,vlen=128,zvkb=true,zba=true,zbb=true,zbc=true,zbs=true,zicbom=true,zicboz=true,zihintpause=true \
	  -nographic \
	  -kernel $(KERNEL_SMODE) \
	  -netdev user,id=n0,hostfwd=tcp::7076-:7076,hostfwd=tcp::7075-:7075 \
	  -device virtio-net-device,netdev=n0 \
	  -drive file=disk.img,if=none,format=raw,id=hd0,cache=unsafe \
	  -device virtio-blk-device,drive=hd0,queue-size=128

run-smode: build-smode disk.img
	qemu-system-riscv64 \
	  -machine virt \
	  -accel tcg,thread=multi \
	  -smp 8 \
	  -m 4G \
	  -cpu rv64,v=true,vlen=128,zvkb=true,zba=true,zbb=true,zbc=true,zbs=true,zicbom=true,zicboz=true,zihintpause=true \
	  -nographic \
	  -kernel $(KERNEL_SMODE) \
	  -netdev user,id=n0,hostfwd=tcp::7076-:7076,hostfwd=tcp::7075-:7075 \
	  -device virtio-net-device,netdev=n0 \
	  -drive file=disk.img,if=none,format=raw,id=hd0,cache=writethrough \
	  -device virtio-blk-device,drive=hd0,queue-size=128

kill:
	pkill -f qemu-system-riscv64

run-crypto-tests:
	$(CARGO) test -p rsnano_sha --target x86_64-unknown-linux-gnu --features simd

run-picocom:
	sudo picocom -b 115200 /dev/ttyUSB0 > orpi.log

# ── Aliases ──────────────────────────────────────────────────────────────────
build: build-smode
run: run-smode

clean:
	$(CARGO) clean
