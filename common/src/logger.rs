use crate::uart;
use core::sync::atomic::{AtomicU8, Ordering};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

/// Log level filter threshold:
/// 0 = OFF, 1 = ERROR, 2 = WARN, 3 = INFO (default), 4 = DEBUG, 5 = TRACE
static MAX_LOG_LEVEL: AtomicU8 = AtomicU8::new(3);

#[inline]
const fn level_to_u8(level: &Level) -> u8 {
    match *level {
        Level::ERROR => 1,
        Level::WARN => 2,
        Level::INFO => 3,
        Level::DEBUG => 4,
        Level::TRACE => 5,
    }
}

/// Sets the maximum log level from a configuration string (`"off"`, `"error"`, `"warn"`,
/// `"info"`, `"debug"`, `"trace"`). Defaults to `"info"` if unrecognized.
pub fn set_log_level(level: &str) {
    let trimmed = level.trim();
    let code = if trimmed.eq_ignore_ascii_case("off") || trimmed.eq_ignore_ascii_case("none") {
        0
    } else if trimmed.eq_ignore_ascii_case("error") {
        1
    } else if trimmed.eq_ignore_ascii_case("warn") || trimmed.eq_ignore_ascii_case("warning") {
        2
    } else if trimmed.eq_ignore_ascii_case("info") {
        3
    } else if trimmed.eq_ignore_ascii_case("debug") {
        4
    } else if trimmed.eq_ignore_ascii_case("trace") {
        5
    } else {
        3
    };
    MAX_LOG_LEVEL.store(code, Ordering::Relaxed);
}

pub struct UartSubscriber;

impl Subscriber for UartSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        level_to_u8(metadata.level()) <= MAX_LOG_LEVEL.load(Ordering::Relaxed)
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        if !self.enabled(event.metadata()) {
            return;
        }
        let _intr = uart::InterruptGuard::new();
        let _guard = uart::UART_LOCK.lock();
        let mut visitor = UartVisitor;
        event.record(&mut visitor);
        uart::print_no_lock(b"\n");
    }

    fn enter(&self, _span: &Id) {}
    fn exit(&self, _span: &Id) {}
}

struct UartVisitor;

impl tracing::field::Visit for UartVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn core::fmt::Debug) {
        use core::fmt::Write;
        if field.name() == "message" {
            let mut writer = uart::UartWriter;
            let _ = write!(writer, "{:?}", value);
        } else {
            let mut writer = uart::UartWriter;
            let _ = write!(writer, " {}={:?}", field.name(), value);
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            uart::print_no_lock(value.as_bytes());
        } else {
            uart::print_no_lock(b" ");
            uart::print_no_lock(field.name().as_bytes());
            uart::print_no_lock(b"=");
            uart::print_no_lock(value.as_bytes());
        }
    }
}

pub fn init() {
    let _ = tracing::subscriber::set_global_default(UartSubscriber);
}
