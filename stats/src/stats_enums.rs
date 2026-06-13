use num_derive::FromPrimitive;
use serde::Serialize;

/// Primary statistics type
#[repr(u8)]
#[derive(FromPrimitive, Serialize, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "snake_case")]
pub enum StatType {
    Error,
    Message,
    Ledger,
    Rollback,
    Network,
    VoteProcessor,
    VoteProcessorTier,
    VoteProcessorOverfill,
    VoteRebroadcaster,
    Election,
    HttpCallbacks,
    Tcp,
    TcpServer,
    TcpChannels,
    TcpListener,
    TcpListenerRejected,
    ConfirmationHeight,
    ConfirmationObserver,
    ConfirmingSet,
    Drop,
    Aggregator,
    Requests,
    RequestAggregator,
    RequestAggregatorVote,
    RequestAggregatorReplies,
    Filter,
    Telemetry,
    VoteGenerator,
    VoteGeneratorFinal,
    VoteCache,
    VoteCacheProcessor,
    Hinting,
    BlockProcessor,
    BlockProcessorSource,
    BlockProcessorResult,
    BlockProcessorOverfill,
    Bootstrap,
    BootstrapVerify,
    BootstrapVerifyBlocks,
    BootstrapVerifyFrontiers,
    BootstrapProcess,
    BootstrapRequest,
    BootstrapRequestBlocks,
    BootstrapReply,
    BootstrapNext,
    BootstrapFrontiers,
    BootstrapAccountSets,
    BootstrapFrontierScan,
    BootstrapTimeout,
    BootstrapServer,
    BootstrapServerRequest,
    BootstrapServerOverfill,
    BootstrapServerResponse,
    Active,
    ActiveElections,
    ActiveElectionsStarted,
    ActiveElectionsConfirmed,
    ActiveElectionsDropped,
    ActiveElectionsTimeout,
    ActiveElectionsCancelled,
    ActiveTimeout,
    Backlog,
    BoundedBacklog,
    Unchecked,
    ElectionScheduler,
    ElectionBucket,
    OptimisticScheduler,
    Handshake,
    RepCrawler,
    LocalBlockBroadcaster,
    RepTiers,
    SynCookies,
    PeerHistory,
    ProcessConfirmed,
    Pruning,
}

impl StatType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Message => "message",
            Self::Ledger => "ledger",
            Self::Rollback => "rollback",
            Self::Network => "network",
            Self::VoteProcessor => "vote_processor",
            Self::VoteProcessorTier => "vote_processor_tier",
            Self::VoteProcessorOverfill => "vote_processor_overfill",
            Self::VoteRebroadcaster => "vote_rebroadcaster",
            Self::Election => "election",
            Self::HttpCallbacks => "http_callbacks",
            Self::Tcp => "tcp",
            Self::TcpServer => "tcp_server",
            Self::TcpChannels => "tcp_channels",
            Self::TcpListener => "tcp_listener",
            Self::TcpListenerRejected => "tcp_listener_rejected",
            Self::ConfirmationHeight => "confirmation_height",
            Self::ConfirmationObserver => "confirmation_observer",
            Self::ConfirmingSet => "confirming_set",
            Self::Drop => "drop",
            Self::Aggregator => "aggregator",
            Self::Requests => "requests",
            Self::RequestAggregator => "request_aggregator",
            Self::RequestAggregatorVote => "request_aggregator_vote",
            Self::RequestAggregatorReplies => "request_aggregator_replies",
            Self::Filter => "filter",
            Self::Telemetry => "telemetry",
            Self::VoteGenerator => "vote_generator",
            Self::VoteGeneratorFinal => "vote_generator_final",
            Self::VoteCache => "vote_cache",
            Self::VoteCacheProcessor => "vote_cache_processor",
            Self::Hinting => "hinting",
            Self::BlockProcessor => "block_processor",
            Self::BlockProcessorSource => "block_processor_source",
            Self::BlockProcessorResult => "block_processor_result",
            Self::BlockProcessorOverfill => "block_processor_overfill",
            Self::Bootstrap => "bootstrap",
            Self::BootstrapVerify => "bootstrap_verify",
            Self::BootstrapVerifyBlocks => "bootstrap_verify_blocks",
            Self::BootstrapVerifyFrontiers => "bootstrap_verify_frontiers",
            Self::BootstrapProcess => "bootstrap_process",
            Self::BootstrapRequest => "bootstrap_request",
            Self::BootstrapRequestBlocks => "bootstrap_request_blocks",
            Self::BootstrapReply => "bootstrap_reply",
            Self::BootstrapNext => "bootstrap_next",
            Self::BootstrapFrontiers => "bootstrap_frontiers",
            Self::BootstrapAccountSets => "bootstrap_account_sets",
            Self::BootstrapFrontierScan => "bootstrap_frontier_scan",
            Self::BootstrapTimeout => "bootstrap_timeout",
            Self::BootstrapServer => "bootstrap_server",
            Self::BootstrapServerRequest => "bootstrap_server_request",
            Self::BootstrapServerOverfill => "bootstrap_server_overfill",
            Self::BootstrapServerResponse => "bootstrap_server_response",
            Self::Active => "active",
            Self::ActiveElections => "active_elections",
            Self::ActiveElectionsStarted => "active_elections_started",
            Self::ActiveElectionsConfirmed => "active_elections_confirmed",
            Self::ActiveElectionsDropped => "active_elections_dropped",
            Self::ActiveElectionsTimeout => "active_elections_timeout",
            Self::ActiveElectionsCancelled => "active_elections_cancelled",
            Self::ActiveTimeout => "active_timeout",
            Self::Backlog => "backlog",
            Self::BoundedBacklog => "bounded_backlog",
            Self::Unchecked => "unchecked",
            Self::ElectionScheduler => "election_scheduler",
            Self::ElectionBucket => "election_bucket",
            Self::OptimisticScheduler => "optimistic_scheduler",
            Self::Handshake => "handshake",
            Self::RepCrawler => "rep_crawler",
            Self::LocalBlockBroadcaster => "local_block_broadcaster",
            Self::RepTiers => "rep_tiers",
            Self::SynCookies => "syn_cookies",
            Self::PeerHistory => "peer_history",
            Self::ProcessConfirmed => "process_confirmed",
            Self::Pruning => "pruning",
        }
    }
}

// Optional detail type
#[repr(u16)]
#[derive(FromPrimitive, Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "snake_case")]
pub enum DetailType {
    // common
    All = 0,
    Ok,
    Loop,
    LoopCleanup,
    Process,
    Processed,
    Ignored,
    Update,
    Updated,
    Inserted,
    Erased,
    Request,
    RequestFailed,
    RequestSuccess,
    Broadcast,
    Cleanup,
    Top,
    None,
    Success,
    Unknown,
    Cache,
    Rebroadcast,
    QueueOverflow,
    Triggered,
    Notify,
    Duplicate,
    Confirmed,
    Unconfirmed,
    Cemented,
    Cooldown,
    Recovered,
    Empty,
    Done,
    Retry,
    Prioritized,
    Pending,
    Sync,
    Requeued,
    Evicted,
    Sent,

    // processing queue
    Queue,
    Overfill,
    Batch,

    // error specific
    InsufficientWork,
    HttpCallback,
    UnreachableHost,
    InvalidNetwork,
    Conflict,

    // confirmation_observer specific
    ActiveQuorum,
    ActiveConfHeight,
    InactiveConfHeight,

    // ledger, block, bootstrap
    Send,
    Receive,
    Open,
    Change,
    StateBlock,
    EpochBlock,
    Fork,
    Old,
    GapPrevious,
    GapSource,
    Rollback,
    RollbackFailed,
    Progress,
    BadSignature,
    NegativeSpend,
    Unreceivable,
    GapEpochOpenPending,
    OpenedBurnAccount,
    BalanceMismatch,
    RepresentativeMismatch,
    BlockPosition,

    // block source
    Live,
    LiveOriginator,
    Bootstrap,
    BootstrapLegacy,
    Unchecked,
    Local,
    Forced,
    Election,

    // message specific
    NotAType,
    Invalid,
    Keepalive,
    Publish,
    ConfirmReq,
    ConfirmAck,
    NodeIdHandshake,
    TelemetryReq,
    TelemetryAck,
    AscPullReq,
    AscPullAck,

    // dropped messages
    ConfirmAckZeroAccount,

    // bootstrap, callback
    Initiate,
    InitiateLegacyAge,
    InitiateLazy,
    InitiateWalletLazy,

    // bootstrap specific
    BulkPull,
    BulkPullAccount,
    BulkPullErrorStartingRequest,
    BulkPullFailedAccount,
    BulkPullRequestFailure,
    BulkPush,
    FrontierReq,
    FrontierConfirmationFailed,
    ErrorSocketClose,

    // vote result
    Vote,
    Valid,
    Replay,
    Indeterminate,

    // vote processor
    VoteOverflow,
    VoteIgnored,

    // election specific
    VoteNew,
    VoteProcessed,
    VoteCached,
    ElectionRestart,
    ElectionNotConfirmed,
    ElectionHintedOverflow,
    ElectionHintedConfirmed,
    ElectionHintedDrop,
    BroadcastVoteNormal,
    BroadcastVoteFinal,
    GenerateVote,
    GenerateVoteNormal,
    GenerateVoteFinal,
    BroadcastBlockInitial,
    BroadcastBlockRepeat,
    ConfirmOnce,
    ConfirmationRequest,

    // election types
    Manual,
    Priority,
    Hinted,
    Optimistic,

    // received messages
    InvalidHeader,
    InvalidMessageType,
    InvalidKeepaliveMessage,
    InvalidPublishMessage,
    InvalidConfirmReqMessage,
    InvalidConfirmAckMessage,
    InvalidNodeIdHandshakeMessage,
    InvalidTelemetryReqMessage,
    InvalidTelemetryAckMessage,
    InvalidBulkPullMessage,
    InvalidBulkPullAccountMessage,
    InvalidFrontierReqMessage,
    InvalidAscPullReqMessage,
    InvalidAscPullAckMessage,
    MessageSizeTooBig,
    OutdatedVersion,

    // network
    LoopKeepalive,
    LoopReachout,
    LoopReachoutCached,
    MergePeer,
    ReachoutLive,
    ReachoutCached,

    // traffic
    Generic,
    BootstrapServer,
    BootstrapRequests,
    BlockBroadcast,
    BlockBroadcastRpc,
    BlockBroadcastInitial,
    ConfirmationRequests,
    VoteRebroadcast,
    RepCrawler,
    VoteReply,
    Telemetry,

    // tcp
    TcpWriteDrop,
    TcpWriteNoSocketDrop,
    TcpSilentConnectionDrop,
    TcpConnectError,

    // tcp_channels
    ChannelAccepted,
    Outdated,

    // tcp_server
    HandshakeAbort,
    HandshakeError,

    // ipc
    Invocations,

    // confirmation height
    BlocksConfirmed,

    // request aggregator
    AggregatorAccepted,
    AggregatorDropped,

    // requests
    RequestsCachedHashes,
    RequestsGeneratedHashes,
    RequestsCachedVotes,
    RequestsGeneratedVotes,
    RequestsCannotVote,
    RequestsUnknown,
    RequestsNonFinal,
    RequestsFinal,

    // request_aggregator
    RequestHashes,
    OverfillHashes,
    NormalVote,
    FinalVote,

    // duplicate
    DuplicatePublishMessage,
    DuplicateConfirmAckMessage,

    // telemetry
    InvalidSignature,
    NodeIdMismatch,
    GenesisMismatch,
    RequestWithinProtectionCacheZone,
    NoResponseReceived,
    UnsolicitedTelemetryAck,
    FailedSendTelemetryReq,
    EmptyPayload,
    CleanupOutdated,

    // vote generator
    GeneratorBroadcasts,
    GeneratorReplies,
    GeneratorRepliesDiscarded,
    GeneratorSpacing,
    SentPr,
    SentNonPr,

    // hinting
    MissingBlock,
    DependentUnconfirmed,
    AlreadyConfirmed,
    Activate,
    ActivateImmediate,
    DependentActivated,

    // bootstrap server
    Response,
    WriteError,
    Blocks,
    ChannelFull,
    Frontiers,
    AccountInfo,

    // backlog
    Activated,
    ActivateFailed,
    ActivateSkip,
    ActivateFull,
    Scanned,

    // active
    Insert,
    InsertFailed,
    TransitionPriority,
    TransitionPriorityFailed,
    ElectionCleanup,
    ActivateImmediately,

    // active_elections
    Started,
    ConfirmDependent,

    // unchecked
    Put,
    Satisfied,
    Trigger,

    // election scheduler
    InsertManual,
    InsertPriority,
    InsertPrioritySuccess,
    EraseOldest,

    // handshake
    InvalidNodeId,
    MissingCookie,
    InvalidGenesis,

    // bootstrap
    MissingTag,
    Reply,
    Throttled,
    Track,
    Timeout,
    NothingNew,
    AccountInfoEmpty,
    LoopDatabase,
    LoopDependencies,
    LoopFrontiers,
    LoopFrontiersProcessing,
    DuplicateRequest,
    InvalidResponseType,
    InvalidResponse,
    TimestampReset,
    ProcessingFrontiers,
    SyncAccounts,

    Prioritize,
    PrioritizeFailed,
    Block,
    BlockFailed,
    Unblock,
    UnblockFailed,
    DependencyUpdate,
    DependencyUpdateFailed,

    NextByRequests,
    NextByTimestamp,
    Advance,
    AdvanceFailed,

    NextNone,
    NextDatabase,
    NextBlocking,
    NextDependency,
    NextFrontier,

    BlockingInsert,
    BlockingOverflow,
    PriorityInsert,
    PrioritySet,
    PriorityErase,
    PriorityUnblocked,
    EraseByThreshold,
    EraseByBlocking,
    PriorityEraseThreshold,
    PriorityEraseBlock,
    PriorityOverflow,
    Deprioritize,
    DeprioritizeFailed,
    SyncDependencies,
    BlockingDecayed,
    DependencySynced,

    RequestBlocks,
    RequestAccountInfo,

    Safe,
    Base,

    // active
    StartedHinted,
    StartedOptimistic,

    // rep_crawler
    ChannelDead,
    QueryChannelBusy,
    QuerySent,
    QueryDuplicate,
    RepTimeout,
    QueryTimeout,
    QueryCompletion,
    CrawlAggressive,
    CrawlNormal,

    // block broadcaster
    BroadcastNormal,
    BroadcastAggressive,
    EraseOld,
    EraseConfirmed,

    // rep tiers
    Tier1,
    Tier2,
    Tier3,

    // confirming_set
    NotifyCemented,
    NotifyAlreadyCemented,
    NotifyIntermediate,
    AlreadyCemented,
    Cementing,
    CementedHash,
    CementingFailed,

    // election_state
    Passive,
    Active,
    ExpiredConfirmed,
    ExpiredUnconfirmed,
    Cancelled,

    // election_status_type
    Ongoing,
    ActiveConfirmedQuorum,
    ActiveConfirmationHeight,
    InactiveConfirmationHeight,

    // query_type
    BlocksByHash,
    BlocksByAccount,
    AccountInfoByHash,

    // bounded backlog
    GatheredTargets,
    PerformingRollbacks,
    NoTargets,
    RollbackMissingBlock,
    RollbackSkipped,
    LoopScan,

    // pruning
    LedgerPruning,
    PruningTarget,
    PrunedCount,
    CollectTargets,

    // vote rebroadcaster
    RebroadcastHashes,
}

impl DetailType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Ok => "ok",
            Self::Process => "process",
            Self::Processed => "processed",
            Self::Ignored => "ignored",
            Self::Update => "update",
            Self::Updated => "updated",
            Self::Inserted => "inserted",
            Self::Success => "success",
            Self::Unknown => "unknown",
            Self::Duplicate => "duplicate",
            Self::Confirmed => "confirmed",
            Self::Cemented => "cemented",
            Self::Done => "done",
            Self::Send => "send",
            Self::Receive => "receive",
            Self::Open => "open",
            Self::Change => "change",
            Self::StateBlock => "state_block",
            Self::EpochBlock => "epoch_block",
            Self::Fork => "fork",
            Self::Old => "old",
            Self::GapPrevious => "gap_previous",
            Self::GapSource => "gap_source",
            Self::Rollback => "rollback",
            Self::Progress => "progress",
            Self::BadSignature => "bad_signature",
            Self::Live => "live",
            Self::Bootstrap => "bootstrap",
            Self::Unchecked => "unchecked",
            Self::Local => "local",
            Self::Election => "election",
            Self::Keepalive => "keepalive",
            Self::Publish => "publish",
            Self::ConfirmReq => "confirm_req",
            Self::ConfirmAck => "confirm_ack",
            Self::AscPullReq => "asc_pull_req",
            Self::AscPullAck => "asc_pull_ack",
            Self::Initiate => "initiate",
            Self::BulkPull => "bulk_pull",
            Self::FrontierReq => "frontier_req",
            Self::Vote => "vote",
            Self::Valid => "valid",
            Self::Replay => "replay",
            Self::Indeterminate => "indeterminate",
            Self::VoteProcessed => "vote_processed",
            Self::ElectionRestart => "election_restart",
            Self::BlocksConfirmed => "blocks_confirmed",
            Self::RequestHashes => "request_hashes",
            Self::Response => "response",
            Self::WriteError => "write_error",
            Self::Blocks => "blocks",
            Self::Frontiers => "frontiers",
            Self::AccountInfo => "account_info",
            Self::Scanned => "scanned",
            Self::Queue => "queue",
            Self::Overfill => "overfill",
            Self::ChannelFull => "channel_full",
            Self::Batch => "batch",
            Self::RequestFailed => "request_failed",
            Self::RequestSuccess => "request_success",
            _ => "unknown",
        }
    }
}

/// Direction of the stat. If the direction is irrelevant, use In
#[derive(FromPrimitive, PartialEq, PartialOrd, Eq, Ord, Clone, Copy, Debug, Hash)]
#[repr(u8)]
pub enum Direction {
    In,
    Out,
}

impl Direction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Direction::In => "in",
            Direction::Out => "out",
        }
    }
}

#[repr(u8)]
#[derive(FromPrimitive, Serialize, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Sample {
    ActiveElectionDuration,
    BootstrapTagDuration,
    RepResponseTime,
    VoteGeneratorFinalHashes,
    VoteGeneratorHashes,
}

impl Sample {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ActiveElectionDuration => "active_election_duration",
            Self::BootstrapTagDuration => "bootstrap_tag_duration",
            Self::RepResponseTime => "rep_response_time",
            Self::VoteGeneratorFinalHashes => "vote_generator_final_hashes",
            Self::VoteGeneratorHashes => "vote_generator_hashes",
        }
    }
}
