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
        let mut rng = rsnano_nullable_random::NullableRng::rng();
        let mut bytes = [0u8; 32];
        use rand::RngCore;
        rng.fill_bytes(&mut bytes);
        tracing::info!("Generated Node ID private key starting with: {:02X} {:02X}", bytes[0], bytes[1]);
        Ok(PrivateKey::from_bytes(&bytes))
    }
}
