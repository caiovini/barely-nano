CARGO ?= cargo
OBJCOPY ?= $(or $(shell which llvm-objcopy 2>/dev/null),$(shell find $(HOME)/.rustup -name llvm-objcopy 2>/dev/null | head -n1),riscv64-linux-gnu-objcopy)
MKIMAGE ?= mkimage

.PHONY: build build-smode build-block-test build-spammer boot-scr fastboot-stage run-crypto-tests run-core-tests run-ledger-tests run-messages-tests run-node-tests run-rpc-messages-tests run-rpc-handler-tests run-websocket-messages-tests run-websocket-client-tests run-websocket-server-tests run-picocom run-stress build-stress build-stress-wasm build-lmdb-to-nvme build-nvme-snapshot stress clean

boot.scr: boot.cmd
	$(MKIMAGE) -C none -A riscv -T script -d boot.cmd boot.scr

RUST_LIB_VENDOR := $(shell $(CARGO) --version >/dev/null 2>&1 && rustc --print sysroot)/lib/rustlib/src/rust/library/vendor

build-stress-wasm:
	RUSTC_BOOTSTRAP=1 RUSTFLAGS="-C target-feature=+simd128" $(CARGO) build -Z build-std=core --offline \
		--config 'source.crates-io.replace-with="vendored-sources"' \
		--config 'source.vendored-sources.directory="$(RUST_LIB_VENDOR)"' \
		--manifest-path tools/nano-stress/wasm/Cargo.toml --release --target wasm32-unknown-unknown
	gzip -9 -n -f -c tools/nano-stress/wasm/target/wasm32-unknown-unknown/release/nano_stress_wasm.wasm \
		> tools/nano-stress/wasm/target/wasm32-unknown-unknown/release/nano_stress_wasm.wasm.gz

# ── S-mode (SpacemiT K3 Pico-ITX: 16 GB LPDDR5, 8x X100 + 8x A100) ───────────
# OpenSBI runs in M-mode; our bare-metal node runs in S-mode via U-Boot.
K3_RUSTFLAGS := -C target-feature=+v,+zvl256b,+zba,+zbb,+zbc,+zbs,+zicbom,+zicboz,+zicond,+zihintpause,+zvbb,+zvbc,+zvkb

build-smode: boot.scr build-stress-wasm
	RUSTFLAGS="$(K3_RUSTFLAGS)" \
	$(CARGO) build -p barely-nano --release --target riscv64gc-unknown-none-elf --features simd
	$(OBJCOPY) -O binary target/riscv64gc-unknown-none-elf/release/barely-nano barely-nano.bin

# ── S-mode + deterministic block generation & pipeline test ──────────────────
# Measures deterministic per-stage block time and blocks/sec on bare-metal hardware.
build-block-test: boot.scr build-stress-wasm
	RUSTFLAGS="$(K3_RUSTFLAGS)" \
	$(CARGO) build -p barely-nano --release --target riscv64gc-unknown-none-elf --features simd,block-test
	$(OBJCOPY) -O binary target/riscv64gc-unknown-none-elf/release/barely-nano barely-nano.bin

fastboot-stage: build-smode
	fastboot stage barely-nano.bin

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

# ── Direct LMDB (data.ldb) -> NVMe Migration Tool ────────────────────────────
build-lmdb-to-nvme:
	RUSTFLAGS="" $(CARGO) build --release --manifest-path tools/lmdb-to-nvme/Cargo.toml --target x86_64-unknown-linux-gnu

# ── Direct NVMe <-> Raw Ledger Snapshot Tool ─────────────────────────────────
build-nvme-snapshot:
	RUSTFLAGS="" $(CARGO) build --release --manifest-path tools/nvme-snapshot/Cargo.toml --target x86_64-unknown-linux-gnu

run-stress: build-stress
	tools/nano-stress/target/x86_64-unknown-linux-gnu/release/nano-stress $(if $(ARGS),$(ARGS),--target 10.10.10.2:7075 --rpc http://10.10.10.2:7076 --network dev)

stress: run-stress

# ── Aliases ──────────────────────────────────────────────────────────────────
build: build-smode

clean:
	$(CARGO) clean
