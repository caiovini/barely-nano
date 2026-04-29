use rsnano_core::PrivateKey;

pub(crate) struct NodeIdKeyFile {}

impl NodeIdKeyFile {
    pub fn new_null() -> Self {
        Self {}
    }

    pub fn default() -> Self {
        Self {}
    }

    pub fn initialize(&mut self, _app_path: &str) -> anyhow::Result<PrivateKey> {
        Ok(PrivateKey::from_bytes([0u8; 32]))
    }
}
