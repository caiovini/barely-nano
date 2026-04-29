use rsnano_core::{
    epoch_v1_link, epoch_v2_link,
    utils::UnixMillisTimestamp,
    Account, Amount, Block, BlockDetails, BlockHash, BlockSideband, Epoch, Epochs, Networks,
    PublicKey, SavedBlock, DEV_GENESIS_KEY,
};
use rsnano_work::WorkThresholds;
use spin::Lazy;

static BETA_PUBLIC_KEY_DATA: &str =
    "259A438A8F9F9226130C84D902C237AF3E57C0981C7D709C288046B110D8C8AC";

static DEV_GENESIS_DATA: &str = r###"{
	"type": "open",
	"source": "B0311EA55708D6A53C75CDBF88300259C6D018522FE3D4D0A242E431F9E8B6D0",
	"representative": "xrb_3e3j5tkog48pnny9dmfzj1r16pg8t1e76dz5tmac6iq689wyjfpiij4txtdo",
	"account": "xrb_3e3j5tkog48pnny9dmfzj1r16pg8t1e76dz5tmac6iq689wyjfpiij4txtdo",
	"work": "7b42a00ee91d5810",
	"signature": "ECDA914373A2F0CA1296475BAEE40500A7F0A7AD72A5A80C81D7FAB7F6C802B2CC7DB50F5DD0FB25B2EF11761FA7344A158DD5A700B21BD47DE5BD0F63153A02"
    }"###;

static BETA_GENESIS_DATA: &str = r###"{
	"type": "open",
	"source": "259A438A8F9F9226130C84D902C237AF3E57C0981C7D709C288046B110D8C8AC",
	"representative": "nano_1betag7az9wk6rbis38s1d35hdsycz1bi95xg4g4j148p6afjk7embcurda4",
	"account": "nano_1betag7az9wk6rbis38s1d35hdsycz1bi95xg4g4j148p6afjk7embcurda4",
	"work": "e87a3ce39b43b84c",
	"signature": "BC588273AC689726D129D3137653FB319B6EE6DB178F97421D11D075B46FD52B6748223C8FF4179399D35CB1A8DF36F759325BD2D3D4504904321FAFB71D7602"
    }"###;

static LIVE_GENESIS_DATA: &str = r###"{
	"type": "open",
	"source": "E89208DD038FBB269987689621D52292AE9C35941A7484756ECCED92A65093BA",
	"representative": "xrb_3t6k35gi95xu6tergt6p69ck76ogmitsa8mnijtpxm9fkcm736xtoncuohr3",
	"account": "xrb_3t6k35gi95xu6tergt6p69ck76ogmitsa8mnijtpxm9fkcm736xtoncuohr3",
	"work": "62f05417dd3fb691",
	"signature": "9F0C933C8ADE004D808EA1985FA746A7E95BA2A38F867640F53EC8F180BDFE9E2C1268DEAD7C2664F356E37ABA362BC58E46DBA03E523A7B5A19E4B6EB12BB02"
    }"###;

pub fn get_ledger_constants_stub() -> LedgerConstants {
    LedgerConstants::new(WorkThresholds::publish_dev().clone(), Networks::NanoDevNetwork)
}

pub fn get_dev_genesis_block() -> SavedBlock {
    get_ledger_constants_stub().genesis_block
}

pub const DEV_GENESIS_ACCOUNT: Account = Account::from_bytes([
    0x3B, 0x5D, 0x1A, 0x42, 0x3D, 0x8D, 0xF4, 0x3E, 0x4E, 0x01, 0x83, 0x0F, 0x0B, 0x6E, 0x1A, 0x07,
    0x44, 0x3E, 0x4E, 0x01, 0x83, 0x0F, 0x0B, 0x6E, 0x1A, 0x07, 0x44, 0x3E, 0x4E, 0x01, 0x83, 0x0F
]); // Placeholder

fn parse_block_from_genesis_data(genesis_data: &str) -> anyhow::Result<Block> {
    let block = serde_json::from_str(genesis_data)?;
    Ok(block)
}

#[derive(Clone)]
pub struct LedgerConstants {
    pub work: WorkThresholds,
    pub genesis_block: SavedBlock,
    pub genesis_account: Account,
    pub genesis_amount: Amount,
    pub burn_account: Account,
    pub epochs: Epochs,
}

pub fn genesis_sideband(genesis_account: Account) -> BlockSideband {
    BlockSideband {
        height: 1,
        timestamp: UnixMillisTimestamp::ZERO,
        account: genesis_account,
        balance: Amount::MAX,
        details: BlockDetails::new(Epoch::Epoch0, false, false, false),
        source_epoch: Epoch::Epoch0,
    }
}

impl LedgerConstants {
    pub fn new(work: WorkThresholds, network: Networks) -> Self {
        let dev_genesis_block = parse_block_from_genesis_data(DEV_GENESIS_DATA).unwrap();
        let beta_genesis_block = parse_block_from_genesis_data(BETA_GENESIS_DATA).unwrap();
        let live_genesis_block = parse_block_from_genesis_data(LIVE_GENESIS_DATA).unwrap();

        let genesis_block = match network {
            Networks::NanoDevNetwork => dev_genesis_block,
            Networks::NanoBetaNetwork => beta_genesis_block,
            Networks::NanoLiveNetwork => live_genesis_block,
            _ => dev_genesis_block, // Fallback
        };
        let genesis_account = genesis_block.account_field().unwrap();

        let nano_beta_account = Account::decode_hex(BETA_PUBLIC_KEY_DATA).unwrap();

        let mut epochs = Epochs::new();

        let epoch_1_signer = PublicKey::from(genesis_account);
        let epoch_link_v1 = epoch_v1_link();

        let nano_live_epoch_v2_signer = Account::decode_account(
            "nano_3qb6o6i1tkzr6jwr5s7eehfxwg9x6eemitdinbpi7u8bjjwsgqfj4wzser3x",
        )
        .unwrap();
        let epoch_2_signer = match network {
            Networks::NanoDevNetwork => DEV_GENESIS_KEY.public_key(),
            Networks::NanoBetaNetwork => nano_beta_account.into(),
            Networks::NanoLiveNetwork => nano_live_epoch_v2_signer.into(),
            _ => DEV_GENESIS_KEY.public_key(),
        };
        let epoch_link_v2 = epoch_v2_link();

        epochs.add(Epoch::Epoch1, epoch_1_signer, epoch_link_v1);
        epochs.add(Epoch::Epoch2, epoch_2_signer, epoch_link_v2);

        let genesis_block = SavedBlock::new(genesis_block, genesis_sideband(genesis_account));

        Self {
            work,
            genesis_block,
            genesis_account,
            genesis_amount: Amount::raw(u128::MAX),
            burn_account: Account::zero(),
            epochs,
        }
    }

    pub fn live() -> Self {
        Self::new(
            WorkThresholds::publish_full().clone(),
            Networks::NanoLiveNetwork,
        )
    }

    pub fn beta() -> Self {
        Self::new(
            WorkThresholds::publish_beta().clone(),
            Networks::NanoBetaNetwork,
        )
    }

    pub fn test() -> Self {
        Self::new(
            WorkThresholds::publish_test().clone(),
            Networks::NanoTestNetwork,
        )
    }

    pub fn dev() -> Self {
        Self::new(
            WorkThresholds::publish_dev().clone(),
            Networks::NanoDevNetwork,
        )
    }

    pub fn unit_test() -> Self {
        Self::new(WorkThresholds::new_stub(), Networks::NanoDevNetwork)
    }
}

pub const DEV_GENESIS_HASH: BlockHash = BlockHash::from_bytes([
    0xB0, 0x31, 0x1E, 0xA5, 0x57, 0x08, 0xD6, 0xA5, 0x3C, 0x75, 0xCD, 0xBF, 0x88, 0x30, 0x02, 0x59,
    0xC6, 0xD0, 0x18, 0x52, 0x2F, 0xE3, 0xD4, 0xD0, 0xA2, 0x42, 0xE4, 0x31, 0xF9, 0xE8, 0xB6, 0xD0
]);
pub const DEV_GENESIS_PUB_KEY: PublicKey = PublicKey::from_bytes([
    0xB0, 0x31, 0x1E, 0xA5, 0x57, 0x08, 0xD6, 0xA5, 0x3C, 0x75, 0xCD, 0xBF, 0x88, 0x30, 0x02, 0x59,
    0xC6, 0xD0, 0x18, 0x52, 0x2F, 0xE3, 0xD4, 0xD0, 0xA2, 0x42, 0xE4, 0x31, 0xF9, 0xE8, 0xB6, 0xD0
]);

pub static DEV_GENESIS_BLOCK: Lazy<Block> = Lazy::new(|| {
    serde_json::from_str(DEV_GENESIS_DATA).unwrap()
});
