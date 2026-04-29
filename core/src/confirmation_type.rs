use strum_macros::{EnumCount, EnumIter};

/// How a block got confirmed
#[derive(PartialEq, Eq, Debug, Clone, Copy, EnumCount, EnumIter)]
pub enum ConfirmationType {
    /// An election for this block was active and received enough votes
    ActiveConfirmedQuorum,
    /// An election for this block was active, but the block got confirmed indirectly
    /// when a newer block got confirmed
    ActiveConfirmationHeight,
    /// There was no active election for this block. It got confirmed indirectly
    /// when a newer block got confirmed
    InactiveConfirmationHeight,
}

impl ConfirmationType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ActiveConfirmedQuorum => "active_quorum",
            Self::ActiveConfirmationHeight => "active_confirmation_height",
            Self::InactiveConfirmationHeight => "inactive",
        }
    }
}
