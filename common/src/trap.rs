#[cfg(target_arch = "riscv64")]
use core::arch::global_asm;
#[cfg(target_arch = "riscv64")]
use core::sync::atomic::{AtomicBool, Ordering};
use core::sync::atomic::AtomicU64;

#[cfg(target_arch = "riscv64")]
use crate::uart;

// ---- tick counter -------------------------------------------------------

pub static TICK_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(target_arch = "riscv64")]
static PROBE_ACTIVE: AtomicBool = AtomicBool::new(false);
#[cfg(target_arch = "riscv64")]
static PROBE_FAULTED: AtomicBool = AtomicBool::new(false);

// ---- CSR helpers --------------------------------------------------------

#[cfg(target_arch = "riscv64")]
macro_rules! csrw {
    ($csr:literal, $val:expr) => {
        unsafe {
            core::arch::asm!(
                concat!("csrw ", $csr, ", {0}"),
                in(reg) $val as usize,
                options(nostack)
            );
        }
    };
}

#[cfg(target_arch = "riscv64")]
macro_rules! csrs {
    ($csr:literal, $val:expr) => {
        unsafe {
            core::arch::asm!(
                concat!("csrs ", $csr, ", {0}"),
                in(reg) $val as usize,
                options(nostack)
            );
        }
    };
}

#[cfg(target_arch = "riscv64")]
macro_rules! csrr {
    ($csr:literal) => {{
        let val: usize;
        unsafe {
            core::arch::asm!(
                concat!("csrr {0}, ", $csr),
                out(reg) val,
                options(nostack)
            );
        }
        val
    }};
}

// ---- trap vector (assembly) ---------------------------------------------
//
// S-mode uses `sret`.
// .align 2 → 4-byte alignment required by stvec direct mode (bits[1:0] = 0b00).

#[cfg(target_arch = "riscv64")]
global_asm!(
    ".align 2",
    ".option arch, +d",
    ".global trap_vector",
    "trap_vector:",
    "addi sp, sp, -256",
    "sd   x5,  32(sp)",  // Save t0 (x5) first!
    "sd   x1,   0(sp)",
    "addi t0, sp, 256",
    "sd   t0,   8(sp)",  // original sp
    "sd   x3,  16(sp)",
    "sd   x4,  24(sp)",
    "sd   x6,  40(sp)",
    "sd   x7,  48(sp)",
    "sd   x8,  56(sp)",
    "sd   x9,  64(sp)",
    "sd   x10, 72(sp)",
    "sd   x11, 80(sp)",
    "sd   x12, 88(sp)",
    "sd   x13, 96(sp)",
    "sd   x14, 104(sp)",
    "sd   x15, 112(sp)",
    "sd   x16, 120(sp)",
    "sd   x17, 128(sp)",
    "sd   x18, 136(sp)",
    "sd   x19, 144(sp)",
    "sd   x20, 152(sp)",
    "sd   x21, 160(sp)",
    "sd   x22, 168(sp)",
    "sd   x23, 176(sp)",
    "sd   x24, 184(sp)",
    "sd   x25, 192(sp)",
    "sd   x26, 200(sp)",
    "sd   x27, 208(sp)",
    "sd   x28, 216(sp)",
    "sd   x29, 224(sp)",
    "sd   x30, 232(sp)",
    "sd   x31, 240(sp)",
    "mv   a0, sp",
    "call rust_trap_handler",
    "ld   x1,   0(sp)",
    "ld   x3,  16(sp)",
    "ld   x4,  24(sp)",
    "ld   x5,  32(sp)",
    "ld   x6,  40(sp)",
    "ld   x7,  48(sp)",
    "ld   x8,  56(sp)",
    "ld   x9,  64(sp)",
    "ld   x10, 72(sp)",
    "ld   x11, 80(sp)",
    "ld   x12, 88(sp)",
    "ld   x13, 96(sp)",
    "ld   x14, 104(sp)",
    "ld   x15, 112(sp)",
    "ld   x16, 120(sp)",
    "ld   x17, 128(sp)",
    "ld   x18, 136(sp)",
    "ld   x19, 144(sp)",
    "ld   x20, 152(sp)",
    "ld   x21, 160(sp)",
    "ld   x22, 168(sp)",
    "ld   x23, 176(sp)",
    "ld   x24, 184(sp)",
    "ld   x25, 192(sp)",
    "ld   x26, 200(sp)",
    "ld   x27, 208(sp)",
    "ld   x28, 216(sp)",
    "ld   x29, 224(sp)",
    "ld   x30, 232(sp)",
    "ld   x31, 240(sp)",
    "addi sp, sp, 256",
    "sret",   // S-mode return
);

// ---- init ---------------------------------------------------------------

#[cfg(target_arch = "riscv64")]
extern "C" {
    fn trap_vector();
}

/// Set up S-mode trap vector and enable FP/Vector CSR bits.
pub fn init() {
    #[cfg(target_arch = "riscv64")]
    {
        let tv = trap_vector as *const () as usize;

        // S-mode CSRs: stvec, sie, sstatus
        // We set up stvec to capture exceptions, but we do NOT configure or enable
        // timer interrupts, because the SpacemiT K1 processor's OpenSBI firmware
        // has a buggy/unimplemented sbi_set_timer ecall that hangs the CPU.
        // The node's scheduler and Sleep futures use rdtime cooperative yielding
        // and do not require preemption, and the watchdog is already continuously
        // kicked in the main loop.
        tracing::debug!("[trap] S-mode trap vector init");
        csrw!("stvec", tv);

        // Enable FS (bits 14:13) and VS (bits 10:9) for vector/floating-point/SIMD ops
        csrs!("sstatus", (3usize << 13) | (3usize << 9));
        tracing::debug!("[trap] S-mode trap vector init done");
    }
}

/// Trap-safe 64-bit read/write memory probe.
/// Returns `true` if `addr` is accessible in S-mode (no PMP/bus/page fault) and holds written data.
pub fn probe_rw_u64(addr: usize) -> bool {
    #[cfg(target_arch = "riscv64")]
    {
        PROBE_FAULTED.store(false, Ordering::SeqCst);
        PROBE_ACTIVE.store(true, Ordering::SeqCst);
        let pattern: u64 = 0xA55A_F00F_DEAD_BEEF ^ (addr as u64);
        let mut readback: u64 = 0;
        // SAFETY: `PROBE_ACTIVE` instructs `rust_trap_handler` to catch any load/store access fault
        // on the 4-byte `.option norvc` `sd`/`ld` instructions and advance `sepc + 4`.
        unsafe {
            core::arch::asm!(
                ".option push",
                ".option norvc",
                "sd {pat}, 0({ptr})",
                "fence rw, rw",
                "ld {out}, 0({ptr})",
                ".option pop",
                ptr = in(reg) addr,
                pat = in(reg) pattern,
                out = inout(reg) readback,
                options(nostack)
            );
        }
        PROBE_ACTIVE.store(false, Ordering::SeqCst);
        !PROBE_FAULTED.load(Ordering::SeqCst) && readback == pattern
    }
    #[cfg(not(target_arch = "riscv64"))]
    {
        let _ = addr;
        false
    }
}

/// Trap-safe 32-bit MMIO/memory read probe.
/// Returns `Some(val)` if `addr` is readable in S-mode (no PMP/bus/page fault), or `None` if it faulted.
pub fn probe_read_u32(addr: usize) -> Option<u32> {
    #[cfg(target_arch = "riscv64")]
    {
        PROBE_FAULTED.store(false, Ordering::SeqCst);
        PROBE_ACTIVE.store(true, Ordering::SeqCst);
        let mut val: u32 = 0;
        // SAFETY: `PROBE_ACTIVE` instructs `rust_trap_handler` to catch any load access fault
        // on the 4-byte `.option norvc` `lw` instruction and advance `sepc + 4`.
        unsafe {
            core::arch::asm!(
                ".option push",
                ".option norvc",
                "lw {out}, 0({ptr})",
                ".option pop",
                ptr = in(reg) addr,
                out = inout(reg) val,
                options(nostack)
            );
        }
        PROBE_ACTIVE.store(false, Ordering::SeqCst);
        if PROBE_FAULTED.load(Ordering::SeqCst) {
            None
        } else {
            Some(val)
        }
    }
    #[cfg(not(target_arch = "riscv64"))]
    {
        let _ = addr;
        None
    }
}

// ---- Rust trap handler --------------------------------------------------

#[cfg(target_arch = "riscv64")]
#[no_mangle]
pub extern "C" fn rust_trap_handler(_regs: *const usize) {
    let xcause = csrr!("scause");

    // MSB = interrupt flag; lower bits = cause code
    let is_interrupt = (xcause >> 63) != 0;
    let cause = xcause & !(1usize << 63);

    if !is_interrupt && PROBE_ACTIVE.load(Ordering::SeqCst) {
        PROBE_FAULTED.store(true, Ordering::SeqCst);
        let sepc = csrr!("sepc");
        csrw!("sepc", sepc + 4);
        return;
    }

    // Timer interrupt cause: 5 in S-mode
    let timer_cause = 5usize;

    if is_interrupt && cause == timer_cause {
        let _tick = TICK_COUNT.fetch_add(1, Ordering::Relaxed) + 1;

        // Kick watchdog on Hart 1 timer ticks
        let mut hart_id: usize = 0;
        unsafe {
            core::arch::asm!("mv {}, tp", out(reg) hart_id);
        }
        if hart_id == 1 {
            crate::watchdog::kick();
        }
    } else {
        // Exception or other interrupt
        let hart_id: usize;
        unsafe { core::arch::asm!("mv {0}, tp", out(reg) hart_id); }
        
        // Use no_lock to avoid deadlocking if the hart was already holding the UART lock
        uart::print_no_lock(b"?");
        uart::print_usize_no_lock(hart_id);
        uart::print_no_lock(b"C=");
        uart::print_hex_no_lock(xcause);
        
        let sepc = csrr!("sepc");
        let stval = csrr!("stval");
        let ra = unsafe { *_regs.add(0) }; // Saved x1 (ra)
        let orig_sp = unsafe { *_regs.add(1) }; // Saved x2 (sp)
        uart::print_no_lock(b" P=");
        uart::print_hex_no_lock(sepc);
        uart::print_no_lock(b" RA=");
        uart::print_hex_no_lock(ra);
        uart::print_no_lock(b" TV=");
        uart::print_hex_no_lock(stval);
        uart::print_no_lock(b" SP=");
        uart::print_hex_no_lock(orig_sp);
        if sepc == 0 {
            uart::print_no_lock(b" (NULL JUMP!)\n");
            loop {}
        }
        uart::print_no_lock(b"\n");
        
        // If it's a critical exception, we must stop
        // 0: Instruction Address Misaligned, 1: Instruction Access Fault, 2: Illegal Instruction,
        // 3: Breakpoint, 4: Load Address Misaligned, 5: Load Access Fault,
        // 6: Store/AMO Address Misaligned, 7: Store/AMO Access Fault, 12-15: Page Faults
        if xcause <= 7 || xcause >= 12 {
            let mut last_print = crate::clint::mtime();
            loop {
                let now = crate::clint::mtime();
                if now.saturating_sub(last_print) > 15 * crate::clint::FREQ {
                    last_print = now;
                    uart::print_no_lock(b"[TRAP] hart=");
                    uart::print_usize_no_lock(hart_id);
                    uart::print_no_lock(b" scause=");
                    uart::print_hex_no_lock(xcause);
                    uart::print_no_lock(b" sepc=");
                    uart::print_hex_no_lock(sepc);
                    uart::print_no_lock(b" ra=");
                    uart::print_hex_no_lock(ra);
                    uart::print_no_lock(b" stval=");
                    uart::print_hex_no_lock(stval);
                    uart::print_no_lock(b" sp=");
                    uart::print_hex_no_lock(orig_sp);
                    uart::print_no_lock(b"\n");
                }
                core::hint::spin_loop();
            }
        }
    }
}
