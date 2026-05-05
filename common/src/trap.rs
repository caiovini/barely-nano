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
    "addi sp, sp, -512",
    "sd   x5,  32(sp)",  // Save t0 (x5) first!
    "sd   x1,   0(sp)",
    "addi t0, sp, 512",
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
    "fsd  f0,  256(sp)",
    "fsd  f1,  264(sp)",
    "fsd  f2,  272(sp)",
    "fsd  f3,  280(sp)",
    "fsd  f4,  288(sp)",
    "fsd  f5,  296(sp)",
    "fsd  f6,  304(sp)",
    "fsd  f7,  312(sp)",
    "fsd  f8,  320(sp)",
    "fsd  f9,  328(sp)",
    "fsd  f10, 336(sp)",
    "fsd  f11, 344(sp)",
    "fsd  f12, 352(sp)",
    "fsd  f13, 360(sp)",
    "fsd  f14, 368(sp)",
    "fsd  f15, 376(sp)",
    "fsd  f16, 384(sp)",
    "fsd  f17, 392(sp)",
    "fsd  f18, 400(sp)",
    "fsd  f19, 408(sp)",
    "fsd  f20, 416(sp)",
    "fsd  f21, 424(sp)",
    "fsd  f22, 432(sp)",
    "fsd  f23, 440(sp)",
    "fsd  f24, 448(sp)",
    "fsd  f25, 456(sp)",
    "fsd  f26, 464(sp)",
    "fsd  f27, 472(sp)",
    "fsd  f28, 480(sp)",
    "fsd  f29, 488(sp)",
    "fsd  f30, 496(sp)",
    "fsd  f31, 504(sp)",
    "mv   a0, sp",
    "call rust_trap_handler",
    "fld  f0,  256(sp)",
    "fld  f1,  264(sp)",
    "fld  f2,  272(sp)",
    "fld  f3,  280(sp)",
    "fld  f4,  288(sp)",
    "fld  f5,  296(sp)",
    "fld  f6,  304(sp)",
    "fld  f7,  312(sp)",
    "fld  f8,  320(sp)",
    "fld  f9,  328(sp)",
    "fld  f10, 336(sp)",
    "fld  f11, 344(sp)",
    "fld  f12, 352(sp)",
    "fld  f13, 360(sp)",
    "fld  f14, 368(sp)",
    "fld  f15, 376(sp)",
    "fld  f16, 384(sp)",
    "fld  f17, 392(sp)",
    "fld  f18, 400(sp)",
    "fld  f19, 408(sp)",
    "fld  f20, 416(sp)",
    "fld  f21, 424(sp)",
    "fld  f22, 432(sp)",
    "fld  f23, 440(sp)",
    "fld  f24, 448(sp)",
    "fld  f25, 456(sp)",
    "fld  f26, 464(sp)",
    "fld  f27, 472(sp)",
    "fld  f28, 480(sp)",
    "fld  f29, 488(sp)",
    "fld  f30, 496(sp)",
    "fld  f31, 504(sp)",
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
    "addi sp, sp, 512",
    "mret",   // M-mode return
);

#[cfg(feature = "smode")]
global_asm!(
    ".align 2",
    ".option arch, +d",
    ".global trap_vector",
    "trap_vector:",
    "addi sp, sp, -512",
    "sd   x5,  32(sp)",  // Save t0 (x5) first!
    "sd   x1,   0(sp)",
    "addi t0, sp, 512",
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
    "fsd  f0,  256(sp)",
    "fsd  f1,  264(sp)",
    "fsd  f2,  272(sp)",
    "fsd  f3,  280(sp)",
    "fsd  f4,  288(sp)",
    "fsd  f5,  296(sp)",
    "fsd  f6,  304(sp)",
    "fsd  f7,  312(sp)",
    "fsd  f8,  320(sp)",
    "fsd  f9,  328(sp)",
    "fsd  f10, 336(sp)",
    "fsd  f11, 344(sp)",
    "fsd  f12, 352(sp)",
    "fsd  f13, 360(sp)",
    "fsd  f14, 368(sp)",
    "fsd  f15, 376(sp)",
    "fsd  f16, 384(sp)",
    "fsd  f17, 392(sp)",
    "fsd  f18, 400(sp)",
    "fsd  f19, 408(sp)",
    "fsd  f20, 416(sp)",
    "fsd  f21, 424(sp)",
    "fsd  f22, 432(sp)",
    "fsd  f23, 440(sp)",
    "fsd  f24, 448(sp)",
    "fsd  f25, 456(sp)",
    "fsd  f26, 464(sp)",
    "fsd  f27, 472(sp)",
    "fsd  f28, 480(sp)",
    "fsd  f29, 488(sp)",
    "fsd  f30, 496(sp)",
    "fsd  f31, 504(sp)",
    "mv   a0, sp",
    "call rust_trap_handler",
    "fld  f0,  256(sp)",
    "fld  f1,  264(sp)",
    "fld  f2,  272(sp)",
    "fld  f3,  280(sp)",
    "fld  f4,  288(sp)",
    "fld  f5,  296(sp)",
    "fld  f6,  304(sp)",
    "fld  f7,  312(sp)",
    "fld  f8,  320(sp)",
    "fld  f9,  328(sp)",
    "fld  f10, 336(sp)",
    "fld  f11, 344(sp)",
    "fld  f12, 352(sp)",
    "fld  f13, 360(sp)",
    "fld  f14, 368(sp)",
    "fld  f15, 376(sp)",
    "fld  f16, 384(sp)",
    "fld  f17, 392(sp)",
    "fld  f18, 400(sp)",
    "fld  f19, 408(sp)",
    "fld  f20, 416(sp)",
    "fld  f21, 424(sp)",
    "fld  f22, 432(sp)",
    "fld  f23, 440(sp)",
    "fld  f24, 448(sp)",
    "fld  f25, 456(sp)",
    "fld  f26, 464(sp)",
    "fld  f27, 472(sp)",
    "fld  f28, 480(sp)",
    "fld  f29, 488(sp)",
    "fld  f30, 496(sp)",
    "fld  f31, 504(sp)",
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
    "addi sp, sp, 512",
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
        // S-mode CSRs: stvec, sie (STIE=bit5), sstatus (SIE=bit1)
        // Timer cause: 5 (supervisor timer interrupt)
        // mtimecmp writes are M-mode only — use SBI set_timer instead
        uart::print_no_lock(b"[trap] stvec\n");
        csrw!("stvec", tv);
        uart::print_no_lock(b"[trap] timer\n");
        sbi::set_timer(clint::mtime_raw() + TIMER_INTERVAL);
        uart::print_no_lock(b"[trap] sie\n");
        csrs!("sie", 1usize << 5);      // STIE
        uart::print_no_lock(b"[trap] sstatus\n");
        // Enable FS (bits 14:13) and VS (bits 10:9)
        csrs!("sstatus", (1usize << 1) | (3usize << 13) | (3usize << 9));  // SIE | FS=Dirty | VS=Dirty
        uart::print_no_lock(b"[trap] done\n");
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
        let tick = TICK_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        if tick % 100 == 0 {
            let hart_id: usize;
            unsafe { core::arch::asm!("mv {0}, tp", out(reg) hart_id); }
            if hart_id < 10 {
                uart::putchar_no_lock(b'!');
                uart::putchar_no_lock(hart_id as u8 + b'0');
            }
        }

        // Schedule next tick
        #[cfg(feature = "mmode")]
        clint::set_mtimecmp(clint::mtime_raw() + TIMER_INTERVAL);
        #[cfg(feature = "smode")]
        sbi::set_timer(clint::mtime_raw() + TIMER_INTERVAL);
    } else {
        // Exception or other interrupt
        let hart_id: usize;
        unsafe { core::arch::asm!("mv {0}, tp", out(reg) hart_id); }
        
        // Use no_lock to avoid deadlocking if the hart was already holding the UART lock
        uart::print_no_lock(b"?");
        if hart_id < 10 {
            uart::putchar_no_lock(hart_id as u8 + b'0');
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
