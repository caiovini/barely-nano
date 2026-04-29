use rsnano_core::{
    Block, BlockDetails, BlockType, Difficulty, DifficultyV1, Epoch, Networks, Root,
    StubDifficulty, WorkNonce,
};
use core::cmp::{max, min};
use alloc::boxed::Box;
use spin::Once;

pub static WORK_THRESHOLDS_STUB: Once<WorkThresholds> = Once::new();

pub struct WorkThresholds {
    pub epoch_1: u64,
    pub epoch_2: u64,
    pub epoch_2_receive: u64,

    // Automatically calculated. The base threshold is the maximum of all thresholds and is used for all work multiplier calculations
    pub base: u64,

    // Automatically calculated. The entry threshold is the minimum of all thresholds and defines the required work to enter the node, but does not guarantee a block is processed
    pub entry: u64,
    pub difficulty: Box<dyn Difficulty>,
}

impl Clone for WorkThresholds {
    fn clone(&self) -> Self {
        Self {
            epoch_1: self.epoch_1,
            epoch_2: self.epoch_2,
            epoch_2_receive: self.epoch_2_receive,
            base: self.base,
            entry: self.entry,
            difficulty: self.difficulty.clone(),
        }
    }
}

impl PartialEq for WorkThresholds {
    fn eq(&self, other: &Self) -> bool {
        self.epoch_1 == other.epoch_1
            && self.epoch_2 == other.epoch_2
            && self.epoch_2_receive == other.epoch_2_receive
            && self.base == other.base
            && self.entry == other.entry
            && self.difficulty.get_difficulty(&Root::default(), 0.into())
                == other.difficulty.get_difficulty(&Root::default(), 0.into())
    }
}

impl core::fmt::Debug for WorkThresholds {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WorkThresholds")
            .field("epoch_1", &self.epoch_1)
            .field("epoch_2", &self.epoch_2)
            .field("epoch_2_receive", &self.epoch_2_receive)
            .field("base", &self.base)
            .field("entry", &self.entry)
            .finish()
    }
}

static PUBLISH_FULL: Once<WorkThresholds> = Once::new();
static PUBLISH_BETA: Once<WorkThresholds> = Once::new();
static PUBLISH_DEV: Once<WorkThresholds> = Once::new();
static PUBLISH_TEST: Once<WorkThresholds> = Once::new();

impl WorkThresholds {
    pub fn publish_full() -> &'static WorkThresholds {
        PUBLISH_FULL.call_once(|| {
            WorkThresholds::new(
                0xffffffc000000000,
                0xfffffff800000000,
                0xfffffe0000000000,
            )
        })
    }

    pub fn publish_beta() -> &'static WorkThresholds {
        PUBLISH_BETA.call_once(|| {
            WorkThresholds::new(
                0xfffff00000000000,
                0xfffff00000000000,
                0xffffe00000000000,
            )
        })
    }

    pub fn publish_dev() -> &'static WorkThresholds {
        PUBLISH_DEV.call_once(|| {
            WorkThresholds::new(
                0xfe00000000000000,
                0xffc0000000000000,
                0xf000000000000000,
            )
        })
    }

    pub fn publish_test() -> &'static WorkThresholds {
        PUBLISH_TEST.call_once(|| {
            WorkThresholds::new(
                0xffffffc000000000,
                0xfffffff800000000,
                0xfffffe0000000000,
            )
        })
    }
}

impl WorkThresholds {
    fn new(epoch_1: u64, epoch_2: u64, epoch_2_receive: u64) -> Self {
        Self::with_difficulty(
            Box::<DifficultyV1>::default(),
            epoch_1,
            epoch_2,
            epoch_2_receive,
        )
    }

    pub fn default_for(network: Networks) -> Self {
        match network {
            Networks::NanoDevNetwork => Self::publish_dev().clone(),
            Networks::NanoBetaNetwork => Self::publish_beta().clone(),
            Networks::NanoLiveNetwork => Self::publish_full().clone(),
            Networks::NanoTestNetwork => Self::none().clone(),
            Networks::Invalid => {
                panic!("no default network set")
            }
        }
    }

    pub fn new_stub() -> Self {
        WorkThresholds::with_difficulty(
            Box::new(StubDifficulty::new()),
            0xfe00000000000000,
            0xffc0000000000000,
            0xf000000000000000,
        )
    }

    pub fn none() -> Self {
        WorkThresholds::with_difficulty(Box::new(DifficultyV1 {}), 0, 0, 0)
    }

    pub fn impossible() -> Self {
        WorkThresholds::with_difficulty(Box::new(DifficultyV1 {}), u64::MAX, u64::MAX, u64::MAX)
    }

    pub fn with_difficulty(
        difficulty: Box<dyn Difficulty>,
        epoch_1: u64,
        epoch_2: u64,
        epoch_2_receive: u64,
    ) -> Self {
        Self {
            epoch_1,
            epoch_2,
            epoch_2_receive,
            base: max(max(epoch_1, epoch_2), epoch_2_receive),
            entry: min(min(epoch_1, epoch_2), epoch_2_receive),
            difficulty,
        }
    }

    pub fn threshold_entry(&self, block_type: BlockType) -> u64 {
        match block_type {
            BlockType::State => self.entry,
            _ => self.epoch_1,
        }
    }

    pub fn threshold(&self, details: &BlockDetails) -> u64 {
        match details.epoch {
            Epoch::Epoch2 => {
                if details.is_receive || details.is_epoch {
                    self.epoch_2_receive
                } else {
                    self.epoch_2
                }
            }
            Epoch::Epoch1 | Epoch::Epoch0 => self.epoch_1,
            _ => {
                u64::MAX
            }
        }
    }

    pub fn threshold_base(&self) -> u64 {
        self.base
    }

    pub fn normalized_multiplier(&self, multiplier: f64, threshold: u64) -> f64 {
        if threshold == self.epoch_1 || threshold == self.epoch_2_receive {
            let ratio = DifficultyV1::to_multiplier(self.epoch_2, threshold);
            (multiplier + (ratio - 1f64)) / ratio
        } else {
            multiplier
        }
    }

    pub fn denormalized_multiplier(&self, multiplier: f64, threshold: u64) -> f64 {
        if threshold == self.epoch_1 || threshold == self.epoch_2_receive {
            let ratio = DifficultyV1::to_multiplier(self.epoch_2, threshold);
            multiplier * ratio + 1f64 - ratio
        } else {
            multiplier
        }
    }

    pub fn difficulty(&self, root: &Root, work: WorkNonce) -> u64 {
        self.difficulty.get_difficulty(root, work)
    }

    pub fn difficulty_block(&self, block: &Block) -> u64 {
        self.difficulty(&block.root(), block.work())
    }

    pub fn validate_entry(&self, root: &Root, work: WorkNonce) -> bool {
        self.difficulty(root, work) >= self.threshold_entry(BlockType::State)
    }

    pub fn validate_entry_block(&self, block: &Block) -> bool {
        let difficulty = self.difficulty_block(block);
        let threshold = self.threshold_entry(block.block_type());
        difficulty >= threshold
    }

    pub fn is_valid_pow(&self, block: &Block, details: &BlockDetails) -> bool {
        self.difficulty_block(block) >= self.threshold(details)
    }
}

pub fn dev_difficulty() -> u64 {
    WorkThresholds::publish_dev().threshold_base()
}
