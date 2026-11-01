CARGO ?= cargo
OBJCOPY ?= $(or $(shell which llvm-objcopy 2>/dev/null),$(shell find $(HOME)/.rustup -name llvm-objcopy 2>/dev/null | head -n1),riscv64-linux-gnu-objcopy)
MKIMAGE ?= mkimage

.PHONY: build build-smode build-k1 build-k3 build-block-test build-spammer boot-scr run-crypto-tests run-core-tests run-ledger-tests run-messages-tests run-node-tests run-rpc-messages-tests run-rpc-handler-tests run-websocket-messages-tests run-websocket-client-tests run-websocket-server-tests run-picocom run-stress build-stress stress clean

boot.scr: boot.cmd
	$(MKIMAGE) -C none -A riscv -T script -d boot.cmd boot.scr

# ── S-mode (default: Orange Pi RV2 / SpacemiT K1 8 GB LPDDR4X) ───────────────
# OpenSBI runs in M-mode; our bare-metal node runs in S-mode via U-Boot.
build-smode: boot.scr
	RUSTFLAGS="-C target-feature=+v,+zvl256b,+zba,+zbb,+zbc,+zbs,+zicbom,+zicboz,+zihintpause" \
	$(CARGO) build -p barely-nano --release --target riscv64gc-unknown-none-elf --features simd,k1
	$(OBJCOPY) -O binary target/riscv64gc-unknown-none-elf/release/barely-nano barely-nano.bin
	@for dir in "/media/cvini/NANO_NODE" "/run/media/cvini/NANO_NODE"; do \
		if [ -d "$$dir" ]; then \
			rm -f "$$dir/barely-nano.bin" "$$dir/config.toml" "$$dir/boot.scr"; \
			sync; \
			cp barely-nano.bin config.toml boot.scr "$$dir/"; \
			sync; \
			echo "Successfully deployed barely-nano.bin, config.toml, and boot.scr to $$dir"; \
		fi; \
	done

build-k1: build-smode

# ── S-mode: SpacemiT K3 (32 GB LPDDR5, 25.00 GB DRAM Bank 1 Buffer Pool) ─────
build-k3: boot.scr
	RUSTFLAGS="-C target-feature=+v,+zvl256b,+zba,+zbb,+zbc,+zbs,+zicbom,+zicboz,+zihintpause" \
	$(CARGO) build -p barely-nano --release --target riscv64gc-unknown-none-elf --no-default-features --features smode,simd,k3
	$(OBJCOPY) -O binary target/riscv64gc-unknown-none-elf/release/barely-nano barely-nano.bin
	@for dir in "/media/cvini/NANO_NODE" "/run/media/cvini/NANO_NODE"; do \
		if [ -d "$$dir" ]; then \
			rm -f "$$dir/barely-nano.bin" "$$dir/config.toml" "$$dir/boot.scr"; \
			sync; \
			cp barely-nano.bin config.toml boot.scr "$$dir/"; \
			sync; \
			echo "Successfully deployed K3 barely-nano.bin, config.toml, and boot.scr to $$dir"; \
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
			rm -f "$$dir/barely-nano.bin" "$$dir/config.toml" "$$dir/boot.scr"; \
			sync; \
			cp barely-nano.bin config.toml boot.scr "$$dir/"; \
			sync; \
			echo "Successfully deployed barely-nano.bin, config.toml, and boot.scr to $$dir"; \
		fi; \
	done

build-spammer: build-block-test

run-crypto-tests:
	RUSTFLAGS="" $(CARGO) test -p rsnano_sha --target x86_64-unknown-linux-gnu --features simd

run-core-tests:
	RUSTFLAGS="" $(CARGO) test -p rsnano_core --target x86_64-unknown-linux-gnu

run-ledger-tests:
	RUSTFLAGS="" $(CARGO) test -p rsnano_ledger --target x86_64-unknown-linux-gnu

run-messages-tests:
	RUSTFLAGS="" $(CARGO) test -p rsnano_messages --target x86_64-unknown-linux-gnu

run-node-tests:
	RUSTFLAGS="" $(CARGO) test -p rsnano_node --target x86_64-unknown-linux-gnu

run-rpc-messages-tests:
	RUSTFLAGS="" $(CARGO) test -p rsnano_rpc_messages --target x86_64-unknown-linux-gnu

run-rpc-handler-tests:
	RUSTFLAGS="" $(CARGO) test -p rsnano_rpc_handler --target x86_64-unknown-linux-gnu

run-websocket-messages-tests:
	RUSTFLAGS="" $(CARGO) test -p rsnano_websocket_messages --target x86_64-unknown-linux-gnu

run-websocket-client-tests:
	RUSTFLAGS="" $(CARGO) test -p rsnano_websocket_client --target x86_64-unknown-linux-gnu

run-websocket-server-tests:
	RUSTFLAGS="" $(CARGO) test -p rsnano_websocket_server --target x86_64-unknown-linux-gnu

run-tests: run-crypto-tests run-core-tests run-ledger-tests run-messages-tests run-node-tests run-rpc-messages-tests run-rpc-handler-tests run-websocket-messages-tests run-websocket-client-tests run-websocket-server-tests

run-picocom:
	sudo picocom -b 115200 /dev/ttyUSB0 > orpi.log

# ── Network Stress Testing (sends blocks to port 7075) ────────────────────────
build-stress:
	RUSTFLAGS="" $(CARGO) build --release --manifest-path tools/nano-stress/Cargo.toml --target x86_64-unknown-linux-gnu

run-stress: build-stress
	tools/nano-stress/target/x86_64-unknown-linux-gnu/release/nano-stress $(if $(ARGS),$(ARGS),--target 192.168.1.200:7075 --rpc http://192.168.1.200:7076 --network dev)

stress: run-stress

# ── Aliases ──────────────────────────────────────────────────────────────────
build: build-smode

clean:
	$(CARGO) clean
