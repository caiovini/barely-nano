const UART0: *mut u8 = 0x1000_0000 as *mut u8;
pub static UART_LOCK: spin::Mutex<()> = spin::Mutex::new(());

pub fn putchar(b: u8) {
    let _lock = UART_LOCK.lock();
    putchar_no_lock(b);
}

pub fn putchar_no_lock(b: u8) {
    unsafe {
        // Wait for transmitter holding register empty (bit 5 of LSR at offset 5)
        while (UART0.add(5).read_volatile() & 0x20) == 0 {}
        UART0.write_volatile(b);
    }
}

pub fn print(s: &[u8]) {
    let _lock = UART_LOCK.lock();
    print_no_lock(s);
}

pub fn print_no_lock(s: &[u8]) {
    for &b in s {
        putchar_no_lock(b);
    }
}

pub fn print_usize(n: usize) {
    let _lock = UART_LOCK.lock();
    print_usize_no_lock(n);
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
    let _lock = UART_LOCK.lock();
    print_usize_hex_no_lock(n);
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
    let _lock = UART_LOCK.lock();
    print_hex_no_lock(n);
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
pub struct UartWriter;
impl core::fmt::Write for UartWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        print_no_lock(s.as_bytes());
        Ok(())
    }
}
