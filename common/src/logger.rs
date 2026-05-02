use tracing::{Level, Metadata, Subscriber, Event};
use tracing::span::{Attributes, Id, Record};
use crate::uart;

use spin::Mutex;
static UART_LOCK: Mutex<()> = Mutex::new(());

pub struct UartSubscriber;

impl Subscriber for UartSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= &Level::INFO && (
            metadata.target().contains("node_monitor") || 
            metadata.target().contains("hello_bare") ||
            metadata.target().contains("rsnano_node") ||
            metadata.target().contains("rsnano_network")
        )
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let _guard = UART_LOCK.lock();
        uart::print(b"[node] ");
        let mut visitor = UartVisitor;
        event.record(&mut visitor);
        uart::print(b"\n");
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
            uart::print(value.as_bytes());
        } else {
            uart::print(b" ");
            uart::print(field.name().as_bytes());
            uart::print(b"=");
            uart::print(value.as_bytes());
        }
    }
}

pub fn init() {
    tracing::subscriber::set_global_default(UartSubscriber).expect("setting default subscriber failed");
}
