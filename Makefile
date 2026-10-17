CARGO ?= cargo
OBJCOPY ?= $(or $(shell which llvm-objcopy 2>/dev/null),$(shell find $(HOME)/.rustup -name llvm-objcopy 2>/dev/null | head -n1),riscv64-linux-gnu-objcopy)
MKIMAGE ?= mkimage

.PHONY: build build-smode build-spammer boot-scr run-crypto-tests run-picocom clean

boot.scr: boot.cmd
	$(MKIMAGE) -C none -A riscv -T script -d boot.cmd boot.scr

# ── S-mode (default: Orange Pi RV2) ──────────────────────────────────────────
# OpenSBI runs in M-mode; our bare-metal node runs in S-mode via U-Boot.
build-smode: boot.scr
	RUSTFLAGS="-C target-feature=+v,+zbb,+zba,+zbs,+zbc,+zihintpause,+zicboz,+zicbom" $(CARGO) build -p barely-nano --release --target riscv64gc-unknown-none-elf --features simd
	$(OBJCOPY) -O binary target/riscv64gc-unknown-none-elf/release/barely-nano barely-nano.bin
	@for dir in "/media/cvini/NANO_NODE" "/run/media/cvini/NANO_NODE"; do \
		if [ -d "$$dir" ]; then \
			cp barely-nano.bin config.toml boot.scr "$$dir/"; \
			sync; \
			echo "Successfully deployed barely-nano.bin, config.toml, and boot.scr to $$dir"; \
		fi; \
	done

# ── S-mode + local block spammer ─────────────────────────────────────────────
# Same as smode but with the local spammer enabled for B-Tree / throughput testing.
build-spammer: boot.scr
	RUSTFLAGS="-C target-feature=+v,+zbb,+zba,+zbs,+zbc,+zihintpause,+zicboz,+zicbom" $(CARGO) build -p barely-nano --release --target riscv64gc-unknown-none-elf --features spammer,simd
	$(OBJCOPY) -O binary target/riscv64gc-unknown-none-elf/release/barely-nano barely-nano.bin
	@for dir in "/media/cvini/NANO_NODE" "/run/media/cvini/NANO_NODE"; do \
		if [ -d "$$dir" ]; then \
			cp barely-nano.bin config.toml boot.scr "$$dir/"; \
			sync; \
			echo "Successfully deployed barely-nano.bin, config.toml, and boot.scr to $$dir"; \
		fi; \
	done

run-crypto-tests:
	$(CARGO) test -p rsnano_sha --target x86_64-unknown-linux-gnu --features simd

run-picocom:
	sudo picocom -b 115200 /dev/ttyUSB0 > orpi.log

# ── Aliases ──────────────────────────────────────────────────────────────────
build: build-smode

clean:
	$(CARGO) clean
