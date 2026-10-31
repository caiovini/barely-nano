use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::any::Any;

use crate::RpcCommandHandler;
use rsnano_core::utils::ContainerInfo;
use rsnano_nullable_clock::Timestamp;
use rsnano_rpc_messages::{StatsArgs, StatsType, SuccessResponse};
use rsnano_stats::StatsLogSink;

impl RpcCommandHandler {
    pub(crate) fn stats(&self, args: StatsArgs) -> anyhow::Result<serde_json::Value> {
        match args.stats_type {
            StatsType::Counters => {
                let mut sink = StatsJsonWriter::new();
                let now = self.node.steady_clock.now();
                sink.write_header("counters", now)?;
                {
                    let stats = self.node.stats();
                    for (key, value) in stats.iter() {
                        sink.write_counter_entry(
                            now,
                            key.stat,
                            key.detail,
                            key.dir.as_str(),
                            *value,
                        )?;
                    }
                }
                sink.finalize();
                sink.add(
                    "stat_duration_seconds",
                    self.node.stats.last_reset().as_secs(),
                );
                Ok(sink.finish())
            }
            StatsType::Samples => {
                let mut sink = StatsJsonWriter::new();
                let now = self.node.steady_clock.now();
                sink.write_header("samples", now)?;
                self.node.stats.log_samples(&mut sink)?;
                sink.add(
                    "stat_duration_seconds",
                    self.node.stats.last_reset().as_secs(),
                );
                Ok(sink.finish())
            }
            StatsType::Database => Ok(serde_json::Value::Object(serde_json::Map::new())),
            StatsType::Objects => Ok(ContainerInfo::builder()
                .node("node", self.node.container_info())
                .finish()
                .into_json()),
        }
    }

    pub(crate) fn stats_clear(&self) -> SuccessResponse {
        self.node.stats.clear();
        SuccessResponse::new()
    }
}

struct StatsJsonWriter {
    tree: serde_json::Map<String, serde_json::Value>,
    entries: Vec<serde_json::Value>,
    log_entries: usize,
}

impl StatsJsonWriter {
    fn new() -> Self {
        Self {
            tree: Default::default(),
            entries: Default::default(),
            log_entries: 0,
        }
    }

    fn add(&mut self, key: impl Into<String>, value: u64) {
        self.tree
            .insert(key.into(), serde_json::Value::String(value.to_string()));
    }

    fn finish(self) -> serde_json::Value {
        serde_json::Value::Object(self.tree)
    }

    fn format_hms(time: Timestamp) -> String {
        let total_secs = (time.millis().max(0) as u64) / 1000;
        let secs = total_secs % 60;
        let mins = (total_secs / 60) % 60;
        let hours = (total_secs / 3600) % 24;
        format!("{:02}:{:02}:{:02}", hours, mins, secs)
    }
}

impl StatsLogSink for StatsJsonWriter {
    fn begin(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn finalize(&mut self) {
        let entries = core::mem::take(&mut self.entries);
        self.tree
            .insert("entries".to_owned(), serde_json::Value::Array(entries));
    }

    fn write_header(&mut self, header: &str, walltime: Timestamp) -> anyhow::Result<()> {
        self.tree.insert(
            "type".to_owned(),
            serde_json::Value::String(header.to_owned()),
        );
        self.tree.insert(
            "created".to_owned(),
            serde_json::Value::String(format!("1970.01.01 {}", Self::format_hms(walltime))),
        );
        Ok(())
    }

    fn write_counter_entry(
        &mut self,
        time: Timestamp,
        entry_type: &str,
        detail: &str,
        dir: &str,
        value: u64,
    ) -> anyhow::Result<()> {
        let mut entry = serde_json::Map::new();
        entry.insert(
            "time".to_owned(),
            serde_json::Value::String(Self::format_hms(time)),
        );
        entry.insert(
            "type".to_owned(),
            serde_json::Value::String(entry_type.to_owned()),
        );
        entry.insert(
            "detail".to_owned(),
            serde_json::Value::String(detail.to_owned()),
        );
        entry.insert("dir".to_owned(), serde_json::Value::String(dir.to_owned()));
        entry.insert(
            "value".to_owned(),
            serde_json::Value::String(value.to_string()),
        );
        self.entries.push(serde_json::Value::Object(entry));
        Ok(())
    }

    fn write_sampler_entry(
        &mut self,
        time: Timestamp,
        sample: &str,
        values: Vec<i64>,
        expected_min_max: (i64, i64),
    ) -> anyhow::Result<()> {
        let mut entry = serde_json::Map::new();
        entry.insert(
            "time".to_owned(),
            serde_json::Value::String(Self::format_hms(time)),
        );
        entry.insert(
            "sample".to_owned(),
            serde_json::Value::String(sample.to_owned()),
        );
        entry.insert(
            "min".to_owned(),
            serde_json::Value::String(expected_min_max.0.to_string()),
        );
        entry.insert(
            "max".to_owned(),
            serde_json::Value::String(expected_min_max.1.to_string()),
        );
        let values_tree = values
            .into_iter()
            .map(|v| serde_json::Value::String(v.to_string()))
            .collect();
        entry.insert("values".to_owned(), serde_json::Value::Array(values_tree));
        self.entries.push(serde_json::Value::Object(entry));
        Ok(())
    }

    fn rotate(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn entries(&self) -> usize {
        self.log_entries
    }

    fn inc_entries(&mut self) {
        self.log_entries += 1;
    }

    fn to_string(&self) -> String {
        serde_json::Value::Object(self.tree.clone()).to_string()
    }

    fn to_object(&self) -> Option<&dyn Any> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{test_rpc_command, test_rpc_command_requires_control};
    use rsnano_rpc_messages::RpcCommand;

    #[test]
    fn stats_counters() {
        let result: serde_json::Value = test_rpc_command(RpcCommand::Stats(StatsArgs {
            stats_type: StatsType::Counters,
        }));
        assert_eq!(result["type"], "counters");
        assert!(result["entries"].is_array());
    }

    #[test]
    fn stats_objects_requires_control() {
        test_rpc_command_requires_control(RpcCommand::Stats(StatsArgs {
            stats_type: StatsType::Objects,
        }));
    }

    #[test]
    fn stats_clear_command() {
        let result: SuccessResponse = test_rpc_command(RpcCommand::StatsClear);
        assert_eq!(result, SuccessResponse::new());
    }
}
