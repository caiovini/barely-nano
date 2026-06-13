const UART0: *mut u8 = 0xD401_7000 as *mut u8;
pub static UART_LOCK: spin::Mutex<()> = spin::Mutex::new(());

use core::sync::atomic::AtomicBool;

static SUPPRESS_LINE: [AtomicBool; 8] = [
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
];

#[inline]
pub fn current_hart_id() -> usize {
    let hart_id: usize;
    unsafe {
        core::arch::asm!("mv {0}, tp", out(reg) hart_id);
    }
    if hart_id < 8 {
        hart_id
    } else {
        0
    }
}

pub fn is_log_suppressed(s: &[u8]) -> bool {
    if s.starts_with(b"[monitor]") || s.starts_with(b"[heartbeat]") {
        return false;
    }
    if s.starts_with(b"[tcp]")
        || s.starts_with(b"[connector]")
        || s.starts_with(b"[dns]")
        || s.starts_with(b"[net]")
        || s.starts_with(b"[test]")
        || s.starts_with(b".[rx]")
        || s.starts_with(b"![tx]")
        || s.starts_with(b"?")
    {
        return true;
    }
    false
}

pub fn putchar(b: u8) {
    if b == b'p' || b == b'.' || b == b'!' {
        return;
    }
    let hart_id = current_hart_id();
    if SUPPRESS_LINE[hart_id].load(core::sync::atomic::Ordering::Relaxed) {
        if b == b'\n' {
            SUPPRESS_LINE[hart_id].store(false, core::sync::atomic::Ordering::Relaxed);
        }
        return;
    }
    let _intr = InterruptGuard::new();
    if let Some(_lock) = UART_LOCK.try_lock() {
        putchar_no_lock(b);
    } else {
        putchar_no_lock(b);
    }
}

pub struct InterruptGuard {
    enabled: bool,
}

impl InterruptGuard {
    pub fn new() -> Self {
        let mut sstatus: usize;
        unsafe {
            core::arch::asm!("csrr {0}, sstatus", out(reg) sstatus);
        }
        let enabled = (sstatus & (1 << 1)) != 0;
        if enabled {
            unsafe {
                core::arch::asm!("csrci sstatus, 2");
            }
        }
        Self { enabled }
    }
}

impl Drop for InterruptGuard {
    fn drop(&mut self) {
        if self.enabled {
            unsafe {
                core::arch::asm!("csrsi sstatus, 2");
            }
        }
    }
}

pub fn putchar_no_lock(b: u8) {
    if b == b'\n' {
        unsafe {
            while (UART0.add(20).read_volatile() & 0x20) == 0 {}
            UART0.write_volatile(b'\r');
        }
    }
    unsafe {
        while (UART0.add(20).read_volatile() & 0x20) == 0 {}
        UART0.write_volatile(b);
    }
}

pub fn blind_write(b: u8) {
    unsafe {
        while (UART0.add(20).read_volatile() & 0x20) == 0 {}
        UART0.write_volatile(b);
    }
}

pub fn print(s: &[u8]) {
    let hart_id = current_hart_id();
    if is_log_suppressed(s) {
        SUPPRESS_LINE[hart_id].store(true, core::sync::atomic::Ordering::Relaxed);
        return;
    }
    if SUPPRESS_LINE[hart_id].load(core::sync::atomic::Ordering::Relaxed) {
        if s.contains(&b'\n') {
            SUPPRESS_LINE[hart_id].store(false, core::sync::atomic::Ordering::Relaxed);
        }
        return;
    }
    let _intr = InterruptGuard::new();
    if let Some(_lock) = UART_LOCK.try_lock() {
        print_no_lock(s);
    } else {
        print_no_lock(s);
    }
}

pub fn print_no_lock(s: &[u8]) {
    for &b in s {
        putchar_no_lock(b);
    }
}

pub fn print_usize(n: usize) {
    let hart_id = current_hart_id();
    if SUPPRESS_LINE[hart_id].load(core::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let _intr = InterruptGuard::new();
    if let Some(_lock) = UART_LOCK.try_lock() {
        print_usize_no_lock(n);
    } else {
        print_usize_no_lock(n);
    }
}

pub fn print_usize_no_lock(mut n: usize) {
    if n == 0 {
        putchar_no_lock(b'0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = 0;
    while n > 0 {
        buf[i] = (n % 10) as u8 + b'0';
        n /= 10;
        i += 1;
    }
    while i > 0 {
        i -= 1;
        putchar_no_lock(buf[i]);
    }
}

pub fn print_usize_hex(n: usize) {
    let hart_id = current_hart_id();
    if SUPPRESS_LINE[hart_id].load(core::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let _intr = InterruptGuard::new();
    if let Some(_lock) = UART_LOCK.try_lock() {
        print_usize_hex_no_lock(n);
    } else {
        print_usize_hex_no_lock(n);
    }
}

pub fn print_usize_hex_no_lock(mut n: usize) {
    if n == 0 {
        putchar_no_lock(b'0');
        return;
    }
    let mut buf = [0u8; 16];
    let mut i = 0;
    while n > 0 {
        let m = (n % 16) as u8;
        buf[i] = if m < 10 { m + b'0' } else { m - 10 + b'a' };
        n /= 16;
        i += 1;
    }
    while i > 0 {
        i -= 1;
        putchar_no_lock(buf[i]);
    }
}

pub fn print_hex(n: usize) {
    let hart_id = current_hart_id();
    if SUPPRESS_LINE[hart_id].load(core::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let _intr = InterruptGuard::new();
    if let Some(_lock) = UART_LOCK.try_lock() {
        print_hex_no_lock(n);
    } else {
        print_hex_no_lock(n);
    }
}

pub fn print_hex_no_lock(mut n: usize) {
    putchar_no_lock(b'0');
    putchar_no_lock(b'x');
    if n == 0 {
        putchar_no_lock(b'0');
        return;
    }
    let mut buf = [0u8; 16];
    let mut i = 0;
    while n > 0 {
        let digit = (n % 16) as u8;
        buf[i] = if digit < 10 {
            b'0' + digit
        } else {
            b'a' + (digit - 10)
        };
        n /= 16;
        i += 1;
    }
    for j in (0..i).rev() {
        putchar_no_lock(buf[j]);
    }
}

pub fn print_i32(n: i32) {
    let hart_id = current_hart_id();
    if SUPPRESS_LINE[hart_id].load(core::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let _intr = InterruptGuard::new();
    if let Some(_lock) = UART_LOCK.try_lock() {
        if n < 0 {
            putchar_no_lock(b'-');
            print_usize_no_lock(n.unsigned_abs() as usize);
        } else {
            print_usize_no_lock(n as usize);
        }
    } else {
        if n < 0 {
            putchar_no_lock(b'-');
            print_usize_no_lock(n.unsigned_abs() as usize);
        } else {
            print_usize_no_lock(n as usize);
        }
    }
}

pub fn print_u8_hex(b: u8) {
    let hart_id = current_hart_id();
    if SUPPRESS_LINE[hart_id].load(core::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let _intr = InterruptGuard::new();
    if let Some(_lock) = UART_LOCK.try_lock() {
        print_u8_hex_no_lock(b);
    } else {
        print_u8_hex_no_lock(b);
    }
}

pub fn print_u8_hex_no_lock(b: u8) {
    let high = b >> 4;
    let low = b & 0x0f;
    putchar_no_lock(if high < 10 { high + b'0' } else { high - 10 + b'a' });
    putchar_no_lock(if low < 10 { low + b'0' } else { low - 10 + b'a' });
}

pub fn try_print(s: &[u8]) -> bool {
    let hart_id = current_hart_id();
    if is_log_suppressed(s) {
        SUPPRESS_LINE[hart_id].store(true, core::sync::atomic::Ordering::Relaxed);
        return false;
    }
    if SUPPRESS_LINE[hart_id].load(core::sync::atomic::Ordering::Relaxed) {
        if s.contains(&b'\n') {
            SUPPRESS_LINE[hart_id].store(false, core::sync::atomic::Ordering::Relaxed);
        }
        return false;
    }
    if let Some(_lock) = UART_LOCK.try_lock() {
        print_no_lock(s);
        true
    } else {
        false
    }
}

pub fn try_print_usize(n: usize) -> bool {
    let hart_id = current_hart_id();
    if SUPPRESS_LINE[hart_id].load(core::sync::atomic::Ordering::Relaxed) {
        return false;
    }
    if let Some(_lock) = UART_LOCK.try_lock() {
        print_usize_no_lock(n);
        true
    } else {
        false
    }
}

pub struct UartWriter;
impl core::fmt::Write for UartWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        print_no_lock(s.as_bytes());
        Ok(())
    }
}
