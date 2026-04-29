use core::any::Any;
use alloc::vec::Vec;
use alloc::string::String;
use rsnano_nullable_clock::Timestamp;

use anyhow::Result;

pub trait StatsLogSink {
    /// Called before logging starts
    fn begin(&mut self) -> Result<()>;

    /// Called after logging is completed
    fn finalize(&mut self);

    /// Write a header entry to the log
    fn write_header(&mut self, header: &str, walltime: Timestamp) -> Result<()>;

    /// Write a counter or sampling entry to the log.
    fn write_counter_entry(
        &mut self,
        time: Timestamp,
        entry_type: &str,
        detail: &str,
        dir: &str,
        value: u64,
    ) -> Result<()>;

    fn write_sampler_entry(
        &mut self,
        time: Timestamp,
        sample: &str,
        values: Vec<i64>,
        expected_min_max: (i64, i64),
    ) -> Result<()>;

    /// Rotates the log (e.g. empty file). This is a no-op for sinks where rotation is not supported.
    fn rotate(&mut self) -> Result<()>;

    /// Returns a reference to the log entry counter
    fn entries(&self) -> usize;

    fn inc_entries(&mut self);

    /// Returns the string representation of the log. If not supported, an empty string is returned.
    fn to_string(&self) -> String;

    /// Returns the object representation of the log result. The type depends on the sink used.
    fn to_object(&self) -> Option<&dyn Any>;
}

// StatsJsonWriter is disabled in no_std for now due to serde_json/chrono dependencies.
// We can implement a lightweight version later if needed.
