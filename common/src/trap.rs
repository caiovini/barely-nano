use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::{clint, uart};
#[cfg(feature = "smode")]
use crate::sbi;

// ---- tick counter -------------------------------------------------------

pub static TICK_COUNT: AtomicU64 = AtomicU64::new(0);

const TIMER_INTERVAL: u64 = clint::FREQ / 10; // 100ms (10 Hz)

// ---- CSR helpers --------------------------------------------------------

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
// Two versions: M-mode uses `mret`, S-mode uses `sret`.
// .align 2 → 4-byte alignment required by xtvec direct mode (bits[1:0] = 0b00).

#[cfg(feature = "mmode")]
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
    "mret",   // M-mode return
);

#[cfg(feature = "smode")]
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

extern "C" {
    fn trap_vector();
}

/// Set up trap vector, enable timer interrupt for the current privilege mode.
pub fn init() {
    let tv = trap_vector as *const () as usize;

    #[cfg(feature = "mmode")]
    {
        // M-mode CSRs: mtvec, mie (MTIE=bit7), mstatus (MIE=bit3)
        // Timer cause: 7 (machine timer interrupt)
        csrw!("mtvec", tv);
        clint::set_mtimecmp(clint::mtime_raw() + TIMER_INTERVAL);
        csrs!("mie", 1usize << 7);      // MTIE
        csrs!("mstatus", 1usize << 3);  // MIE
    }

    #[cfg(feature = "smode")]
    {
        // S-mode CSRs: stvec, sie, sstatus
        // We set up stvec to capture exceptions, but we do NOT configure or enable
        // timer interrupts, because the SpacemiT K1 processor's OpenSBI firmware
        // has a buggy/unimplemented sbi_set_timer ecall that hangs the CPU.
        // The node's scheduler and Sleep futures use rdtime cooperative yielding
        // and do not require preemption, and the watchdog is already continuously
        // kicked in the main loop.
        uart::print(b"[trap] S-mode trap vector init\n");
        csrw!("stvec", tv);
        
        // Enable FS (bits 14:13) and VS (bits 10:9) for vector/floating-point/SIMD ops
        csrs!("sstatus", (3usize << 13) | (3usize << 9));
        uart::print(b"[trap] S-mode trap vector init done\n");
    }
}

// ---- Rust trap handler --------------------------------------------------

#[no_mangle]
pub extern "C" fn rust_trap_handler(_regs: *const usize) {
    // Read the correct cause register for the current privilege mode
    #[cfg(feature = "mmode")]
    let xcause = csrr!("mcause");
    #[cfg(feature = "smode")]
    let xcause = csrr!("scause");

    // MSB = interrupt flag; lower bits = cause code
    let is_interrupt = (xcause >> 63) != 0;
    let cause = xcause & !(1usize << 63);

    // Timer interrupt cause: 7 in M-mode, 5 in S-mode
    #[cfg(feature = "mmode")]
    let timer_cause = 7usize;
    #[cfg(feature = "smode")]
    let timer_cause = 5usize;

    if is_interrupt && cause == timer_cause {
        let _tick = TICK_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        /*if tick % 100 == 0 {
            let hart_id: usize;
            unsafe { core::arch::asm!("mv {0}, tp", out(reg) hart_id); }
            if hart_id < 10 {
                uart::putchar(b'!');
                uart::putchar(hart_id as u8 + b'0');
            }
        }*/

        // Kick watchdog on Hart 1 timer ticks
        let mut hart_id: usize = 0;
        unsafe {
            core::arch::asm!("mv {}, tp", out(reg) hart_id);
        }
        if hart_id == 1 {
            crate::watchdog::kick();
        }

        // Schedule next tick
        #[cfg(feature = "mmode")]
        clint::set_mtimecmp(clint::mtime_raw() + TIMER_INTERVAL);
        #[cfg(feature = "smode")]
        {
            // Do not call sbi::set_timer here since timer interrupts are disabled
        }
    } else {
        // Exception or other interrupt
        let hart_id: usize;
        unsafe { core::arch::asm!("mv {0}, tp", out(reg) hart_id); }
        
        // Use no_lock to avoid deadlocking if the hart was already holding the UART lock
        uart::print_no_lock(b"?");
        if hart_id < 10 {
            uart::putchar(hart_id as u8 + b'0');
        } else {
            uart::print_usize_no_lock(hart_id);
        }
        uart::print_no_lock(b"C=");
        uart::print_hex_no_lock(xcause);
        
        #[cfg(feature = "smode")]
        {
            let sepc = csrr!("sepc");
            let ra = unsafe { *_regs.add(0) }; // Saved x1 (ra)
            uart::print_no_lock(b" P=");
            uart::print_hex_no_lock(sepc);
            uart::print_no_lock(b" RA=");
            uart::print_hex_no_lock(ra);
            if sepc == 0 {
                uart::print_no_lock(b" (NULL JUMP!)\n");
                loop {}
            }
        }
        uart::print_no_lock(b"\n");
        
        // If it's a critical exception, we must stop
        // 0: Instruction Address Misaligned, 1: Instruction Access Fault, 2: Illegal Instruction, 5: Load Access Fault, 12-15: Page Faults
        if xcause == 0 || xcause == 1 || xcause == 2 || xcause == 5 || xcause >= 12 {
             loop {}
        }
    }
}
