use rsnano_network::ChannelId;

pub(super) struct PeerScore {
    pub channel_id: ChannelId,
    /// Number of requests to a peer that haven't been replied to yet
    pub running_queries: usize,
    pub request_count_total: usize,
    pub response_count_total: usize,
    pub score: i32,
}

impl PeerScore {
    pub const INITIAL_SCORE: i32 = 1000;

    pub fn new(channel_id: ChannelId) -> Self {
        Self {
            channel_id,
            running_queries: 1,
            request_count_total: 1,
            response_count_total: 0,
            score: Self::INITIAL_SCORE,
        }
    }

    pub fn decay(&mut self) {
        if self.score < Self::INITIAL_SCORE {
            self.score += 1;
        }
    }
}
