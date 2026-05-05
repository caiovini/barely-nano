#![no_std]
extern crate alloc;

use alloc::string::String;
use alloc::sync::Arc;
use core::str::FromStr;
use rsnano_nullable_env::Env;
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};

#[derive(Default)]
pub struct TracingInitializer {
    is_nulled: bool,
    env: Env,
    init_listener: OutputListenerMt<()>,
}

impl TracingInitializer {
    pub fn new_null() -> Self {
        Self {
            is_nulled: true,
            env: Env::new_null(),
            init_listener: Default::default(),
        }
    }

    pub fn init(&self) {
        self.init_with_default_log_level("info");
    }

    pub fn init_with_default_log_level(&self, default_level: impl Into<String>) {
        self.init_listener.emit(());
        let _dirs = self.log_dirs(default_level);
        let _log_style = self.log_style();

        if !self.is_nulled {
            // In bare metal, we don't have tracing-subscriber.
            // We rely on the tracing/log bridge.
            // The actual logger should be initialized by the kernel (main).
        }
    }

    fn log_dirs(&self, default_level: impl Into<String>) -> String {
        self.env
            .var("RUST_LOG") // EnvFilter::DEFAULT_ENV is usually RUST_LOG
            .unwrap_or(default_level.into())
    }

    fn log_style(&self) -> LogStyle {
        self.env
            .var("NANO_LOG")
            .as_ref()
            .map(|i| i.as_str())
            .unwrap_or_default()
            .parse()
            .unwrap_or(LogStyle::Ansi)
    }

    pub fn track(&self) -> Arc<OutputTrackerMt<()>> {
        self.init_listener.track()
    }
}

enum LogStyle {
    Ansi,
    NoAnsi,
    Json,
}

impl FromStr for LogStyle {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "json" => Ok(LogStyle::Json),
            "noansi" => Ok(LogStyle::NoAnsi),
            _ => Ok(LogStyle::Ansi),
        }
    }
}
