#![no_std]
extern crate alloc;

use alloc::{borrow::ToOwned, string::String, sync::Arc};
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};

pub struct Console {
    is_null: bool,
    output_listener: OutputListenerMt<String>,
}

impl Console {
    fn new(is_null: bool) -> Self {
        Self {
            is_null,
            output_listener: OutputListenerMt::new(),
        }
    }
    pub fn new_null() -> Self {
        Self::new(true)
    }

    pub fn println(&self, line: impl AsRef<str>) {
        let line = line.as_ref();
        if self.output_listener.is_tracked() {
            self.output_listener.emit(line.to_owned());
        }
        if !self.is_null {
            kernel_common::uart::print(line.as_bytes());
            kernel_common::uart::print(b"\n");
        }
    }

    pub fn track(&self) -> Arc<OutputTrackerMt<String>> {
        self.output_listener.track()
    }
}

impl Default for Console {
    fn default() -> Self {
        Self::new(false)
    }
}
