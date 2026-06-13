/// SBI ecall wrappers (used in S-mode when OpenSBI is the M-mode firmware).
///
/// Calling convention:
///   a7 = extension ID, a6 = function ID, a0-a5 = args
///   Returns: a0 = error, a1 = value

/// Get current time in ticks from the RISC-V rdtime instruction.
#[inline]
pub fn get_time() -> u64 {
    let mut time: u64;
    unsafe {
        core::arch::asm!("rdtime {0}", out(reg) time);
    }
    time
}

/// SBI timer extension — set next timer deadline.
/// We use the legacy SBI v0.1 timer extension (EID 0x00) as the modern "TIME" extension
/// (EID 0x54494D45) causes early ecall hangs on the SpacemiT K1 processor's OpenSBI firmware.
#[inline]
pub fn set_timer(stime_value: u64) {
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") 0usize, // Legacy EID 0x00 for sbi_set_timer
            inout("a0") stime_value as usize => _,
            lateout("a1") _,
            options(nostack),
        );
    }
}

/// SBI console putchar (EID 0x01) — one byte over debug console.
#[allow(dead_code)]
#[inline]
pub fn console_putchar(c: u8) {
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") 0x01usize,
            inout("a0") c as usize => _,
            lateout("a1") _,
            options(nostack),
        );
    }
}

/// SBI HSM extension (EID 0x48534D "HSM") — hart state management.
#[inline]
pub fn hart_start(hartid: usize, start_addr: usize, opaque: usize) -> (usize, usize) {
    let err: usize;
    let val: usize;
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") 0x48534Dusize, // "HSM"
            in("a6") 0usize,          // sbi_hart_start
            inout("a0") hartid => err,
            inout("a1") start_addr => val,
            in("a2") opaque,
            options(nostack),
        );
    }
    (err, val)
}

/// SBI HSM extension — get hart status.
#[inline]
pub fn hart_get_status(hartid: usize) -> (usize, usize) {
    let err: usize;
    let val: usize;
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") 0x48534Dusize, // "HSM"
            in("a6") 2usize,          // sbi_hart_get_status
            inout("a0") hartid => err,
            lateout("a1") val,
            options(nostack),
        );
    }
    (err, val)
}

/// SBI IPI extension (EID 0x735049 "sPI") — send software IPI to harts.
/// hart_mask: bitmask of target harts (bit N = hart N relative to hart_mask_base).
/// IPI delivery uses CLINT MMIO (non-cacheable) so the recipient sees it
/// regardless of L1 dcache coherency — correct for cross-hart signalling
/// before L2 snooping is active on this SoC.
#[inline]
pub fn send_ipi(hart_mask: usize, hart_mask_base: usize) -> usize {
    let err: usize;
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") 0x735049usize,  // "sPI" IPI extension EID
            in("a6") 0usize,          // sbi_send_ipi function ID
            inout("a0") hart_mask => err,
            in("a1") hart_mask_base,
            options(nostack),
        );
    }
    err
}

#[inline]
pub fn remote_fence_i(hart_mask: usize, hart_mask_base: usize) -> usize {
    let err: usize;
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") 0x52464E43usize, // "RFNC"
            in("a6") 0usize,          // sbi_remote_fence_i
            inout("a0") hart_mask => err,
            in("a1") hart_mask_base,
            options(nostack),
        );
    }
    err
}
