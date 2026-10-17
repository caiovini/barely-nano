CARGO ?= cargo
OBJCOPY ?= $(or $(shell which llvm-objcopy 2>/dev/null),$(shell find $(HOME)/.rustup -name llvm-objcopy 2>/dev/null | head -n1),riscv64-linux-gnu-objcopy)

.PHONY: build build-smode build-spammer run-crypto-tests run-picocom clean

# ── S-mode (default: Orange Pi RV2) ──────────────────────────────────────────
# OpenSBI runs in M-mode; our bare-metal node runs in S-mode via U-Boot.
build-smode:
	RUSTFLAGS="-C target-feature=+v,+zbb,+zba,+zbs,+zbc,+zihintpause,+zicboz,+zicbom" $(CARGO) build -p barely-nano --release --target riscv64gc-unknown-none-elf --features simd
	$(OBJCOPY) -O binary target/riscv64gc-unknown-none-elf/release/barely-nano barely-nano.bin
	@if [ -d "/media/cvini/NANO_NODE" ]; then \
		cp barely-nano.bin /media/cvini/NANO_NODE/; \
		sync; \
		echo "Successfully deployed barely-nano.bin to SD card"; \
	fi

# ── S-mode + local block spammer ─────────────────────────────────────────────
# Same as smode but with the local spammer enabled for B-Tree / throughput testing.
build-spammer:
	RUSTFLAGS="-C target-feature=+v,+zbb,+zba,+zbs,+zbc,+zihintpause,+zicboz,+zicbom" $(CARGO) build -p barely-nano --release --target riscv64gc-unknown-none-elf --features spammer,simd
	$(OBJCOPY) -O binary target/riscv64gc-unknown-none-elf/release/barely-nano barely-nano.bin
	@if [ -d "/media/cvini/NANO_NODE" ]; then \
		cp barely-nano.bin /media/cvini/NANO_NODE/; \
		sync; \
		echo "Successfully deployed barely-nano.bin to SD card"; \
	fi

run-crypto-tests:
	$(CARGO) test -p rsnano_sha --target x86_64-unknown-linux-gnu --features simd

run-picocom:
	sudo picocom -b 115200 /dev/ttyUSB0 > orpi.log

# ── Aliases ──────────────────────────────────────────────────────────────────
build: build-smode

clean:
	$(CARGO) clean
