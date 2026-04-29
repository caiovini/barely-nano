const UART0: *mut u8 = 0x1000_0000 as *mut u8;

pub fn putchar(b: u8) {
    unsafe { UART0.write_volatile(b) };
}

pub fn print(s: &[u8]) {
    for &b in s {
        putchar(b);
    }
}

pub fn print_usize(mut n: usize) {
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
