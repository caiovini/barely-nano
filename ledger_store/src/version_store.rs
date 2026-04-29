use kernel_common::storage::DB;

pub struct BtreeVersionStore {}
impl BtreeVersionStore {
    pub const TABLE_ID: u8 = 11;
    pub fn new() -> Self { Self {} }
}
