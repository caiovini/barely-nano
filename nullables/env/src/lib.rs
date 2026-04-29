#![no_std]

extern crate alloc;

use alloc::{string::{String, ToString}, vec::Vec};

#[derive(Debug, PartialEq, Eq)]
pub enum VarError {
    NotPresent,
    NotUnicode,
}

pub struct Env {
    is_nulled: bool,
    configured_responses: Vec<(&'static str, &'static str)>,
}

impl Env {
    pub fn var(&self, key: impl AsRef<str>) -> Result<String, VarError> {
        let key = key.as_ref();
        
        // On bare-metal, we have no OS to provide environment variables.
        // Therefore, we ALWAYS act as if we are in a nulled environment,
        // and we only return variables that were explicitly configured in code.
        self.configured_responses
            .iter()
            .find_map(|(k, v)| if *k == key { Some(v.to_string()) } else { None })
            .ok_or(VarError::NotPresent)
    }
}

impl Default for Env {
    fn default() -> Self {
        Self {
            is_nulled: false,
            configured_responses: Vec::new(),
        }
    }
}

impl Env {
    pub fn new_null() -> Self {
        Self {
            is_nulled: true,
            configured_responses: Vec::new(),
        }
    }

    pub fn new_null_with(configured_responses: Vec<(&'static str, &'static str)>) -> Self {
        Self {
            is_nulled: true,
            configured_responses,
        }
    }
}
