CARGO ?= cargo
OBJCOPY ?= $(or $(shell which llvm-objcopy 2>/dev/null),$(shell find $(HOME)/.rustup -name llvm-objcopy 2>/dev/null | head -n1),riscv64-linux-gnu-objcopy)
MKIMAGE ?= mkimage

.PHONY: build build-smode build-block-test build-spammer boot-scr run-crypto-tests run-picocom run-stress build-stress stress clean

boot.scr: boot.cmd
	$(MKIMAGE) -C none -A riscv -T script -d boot.cmd boot.scr

# ── S-mode (default: Orange Pi RV2) ──────────────────────────────────────────
# OpenSBI runs in M-mode; our bare-metal node runs in S-mode via U-Boot.
build-smode: boot.scr
	RUSTFLAGS="-C target-feature=+v,+zvl256b,+zba,+zbb,+zbc,+zbs,+zicbom,+zicboz,+zihintpause" \
	$(CARGO) build -p barely-nano --release --target riscv64gc-unknown-none-elf --features simd
	$(OBJCOPY) -O binary target/riscv64gc-unknown-none-elf/release/barely-nano barely-nano.bin
	@for dir in "/media/cvini/NANO_NODE" "/run/media/cvini/NANO_NODE"; do \
		if [ -d "$$dir" ]; then \
			cp barely-nano.bin config.toml boot.scr "$$dir/"; \
			sync; \
			echo "Successfully deployed barely-nano.bin, config.toml, and boot.scr to $$dir"; \
		fi; \
	done

# ── S-mode + deterministic block generation & pipeline test ──────────────────
# Measures deterministic per-stage block time and blocks/sec on bare-metal hardware.
build-block-test: boot.scr
	RUSTFLAGS="-C target-feature=+v,+zvl256b,+zba,+zbb,+zbc,+zbs,+zicbom,+zicboz,+zihintpause" \
	$(CARGO) build -p barely-nano --release --target riscv64gc-unknown-none-elf --features simd,block-test
	$(OBJCOPY) -O binary target/riscv64gc-unknown-none-elf/release/barely-nano barely-nano.bin
	@for dir in "/media/cvini/NANO_NODE" "/run/media/cvini/NANO_NODE"; do \
		if [ -d "$$dir" ]; then \
			cp barely-nano.bin config.toml boot.scr "$$dir/"; \
			sync; \
			echo "Successfully deployed barely-nano.bin, config.toml, and boot.scr to $$dir"; \
		fi; \
	done

build-spammer: build-block-test

run-crypto-tests:
	$(CARGO) test -p rsnano_sha --target x86_64-unknown-linux-gnu --features simd

run-picocom:
	sudo picocom -b 115200 /dev/ttyUSB0 > orpi.log

# ── Network Stress Testing (sends blocks to port 7075) ────────────────────────
build-stress:
	RUSTFLAGS="" $(CARGO) build --release --manifest-path ../rsnano-node/tools/nano-stress/Cargo.toml --target x86_64-unknown-linux-gnu

run-stress: build-stress
	../rsnano-node/target/x86_64-unknown-linux-gnu/release/nano-stress $(if $(ARGS),$(ARGS),--target 192.168.1.200:7075 --rpc http://192.168.1.200:7076 --network dev)

stress: run-stress

# ── Aliases ──────────────────────────────────────────────────────────────────
build: build-smode

clean:
	$(CARGO) clean
