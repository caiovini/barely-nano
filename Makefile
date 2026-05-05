KERNEL_MMODE = target/riscv64gc-unknown-none-elf/release/hello-bare
KERNEL_SMODE = target/riscv64gc-unknown-none-elf/release/hello-bare
CARGO = /home/cvini/.asdf/shims/cargo

.PHONY: build-mmode build-smode run-mmode run-smode clean

disk.img:
	dd if=/dev/zero of=$@ bs=1M count=64

# ── M-mode ──────────────────────────────────────────────────────────────────
# Runs entirely in machine mode; no firmware needed.
build-mmode:
	$(CARGO) build -p hello-bare --release --target riscv64gc-unknown-none-elf --no-default-features --features mmode

run-mmode: build-mmode disk.img
	qemu-system-riscv64 \
	  -machine virt \
	  -smp 8 \
	  -cpu rv64,v=true,zvkb=true,zbb=true,zicbom=true \
	  -nographic \
	  -bios none \
	  -kernel $(KERNEL_MMODE) \
	  -netdev user,id=n0,hostfwd=tcp::7076-:7076,hostfwd=tcp::7075-:7075 \
	  -device virtio-net-device,netdev=n0 \
	  -drive file=disk.img,if=none,format=raw,id=hd0,cache=writethrough \
	  -device virtio-blk-device,drive=hd0,queue-size=128

# ── S-mode (default) ─────────────────────────────────────────────────────────
# OpenSBI (bundled with QEMU) runs in M-mode; our kernel runs in S-mode.
# This mirrors the exact boot chain used on the Lichee Pi 4A.
build-smode:
	$(CARGO) build -p hello-bare --release --target riscv64gc-unknown-none-elf

run-smode: build-smode disk.img
	qemu-system-riscv64 \
	  -machine virt \
	  -smp 8 \
	  -m 1G \
	  -cpu rv64,v=true,zvkb=true,zbb=true,zicbom=true \
	  -nographic \
	  -kernel $(KERNEL_SMODE) \
	  -netdev user,id=n0,hostfwd=tcp::7076-:7076,hostfwd=tcp::7075-:7075 \
	  -device virtio-net-device,netdev=n0 \
	  -drive file=disk.img,if=none,format=raw,id=hd0,cache=writethrough \
	  -device virtio-blk-device,drive=hd0,queue-size=128

kill:
	pkill -f qemu-system-riscv64

# ── Aliases ──────────────────────────────────────────────────────────────────
build: build-smode
run: run-smode

clean:
	$(CARGO) clean
