use alloc::string::String;
use rsnano_core::Networks;

// TODO - Legacy support for rsnano-node, but we dont use this in the bare metal version.
pub fn working_path_for(_network: Networks) -> Option<String> {
    None
}

pub fn unique_path() -> Option<String> {
    None
}

fn _unique_path_for(_network: Networks) -> Option<String> {
    None
}
