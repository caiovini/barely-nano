const UART0: *mut u8 = 0x1000_0000 as *mut u8;
static UART_LOCK: spin::Mutex<()> = spin::Mutex::new(());

pub fn putchar(b: u8) {
    unsafe {
        // Wait for transmitter holding register empty (bit 5 of LSR at offset 5)
        while (UART0.add(5).read_volatile() & 0x20) == 0 {}
        UART0.write_volatile(b);
    }
}

pub fn print(s: &[u8]) {
    let _lock = UART_LOCK.lock();
    for &b in s {
        putchar(b);
    }
}

pub fn print_usize(mut n: usize) {
    let _lock = UART_LOCK.lock();
    if n == 0 {
        putchar(b'0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = 0;
    while n > 0 {
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    for j in (0..i).rev() {
        putchar(buf[j]);
    }
}

pub fn print_hex(mut n: usize) {
    let _lock = UART_LOCK.lock();
    putchar(b'0');
    putchar(b'x');
    if n == 0 {
        putchar(b'0');
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
        putchar(buf[j]);
    }
}
pub struct UartWriter;
impl core::fmt::Write for UartWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        print(s.as_bytes());
        Ok(())
    }
}
