use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub struct OpenclToml {
    pub device: Option<u32>,
    pub enable: Option<bool>,
    pub platform: Option<u32>,
    pub threads: Option<u32>,
}
