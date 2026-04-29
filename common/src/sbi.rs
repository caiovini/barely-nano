/// SBI ecall wrappers (used in S-mode when OpenSBI is the M-mode firmware).
///
/// Calling convention:
///   a7 = extension ID, a6 = function ID, a0-a5 = args
///   Returns: a0 = error, a1 = value

/// SBI timer extension (EID 0x54494D45 "TIME") — set next timer deadline.
#[inline]
pub fn set_timer(stime_value: u64) {
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") 0x54494D45usize, // "TIME" extension
            in("a6") 0usize,          // sbi_set_timer function
            inout("a0") stime_value as usize => _,
            lateout("a1") _,
            options(nostack),
        );
    }
}

/// Legacy SBI console putchar (EID 0x01) — one byte over debug console.
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
