use alloc::vec::Vec;
use super::{peer_score::PeerScore, peer_score_container::PeerScoreContainer};
use rsnano_core::utils::ContainerInfo;
use rsnano_network::ChannelId;

/// Container for tracking and scoring peers with respect to bootstrapping
pub(crate) struct PeerScoring {
    scoring: PeerScoreContainer,
    channel_limit: usize,
}

impl PeerScoring {
    pub fn new() -> Self {
        Self {
            scoring: PeerScoreContainer::default(),
            channel_limit: 16,
        }
    }

    pub fn set_channel_limit(&mut self, limit: usize) {
        self.channel_limit = limit;
    }

    pub const BAN_THRESHOLD: i32 = 0;

    pub fn punish(&mut self, channel_id: ChannelId, penalty: i32) {
        self.scoring.modify(channel_id, |i| {
            i.score = i.score.saturating_sub(penalty);
        });
    }

    pub fn reward(&mut self, channel_id: ChannelId, reward: i32) {
        self.scoring.modify(channel_id, |i| {
            i.score = core::cmp::min(PeerScore::INITIAL_SCORE, i.score.saturating_add(reward));
        });
    }

    pub fn is_banned(&self, channel_id: ChannelId) -> bool {
        if let Some(score) = self.scoring.get(channel_id) {
            score.score <= Self::BAN_THRESHOLD
        } else {
            false
        }
    }

    pub fn received_message(&mut self, channel_id: ChannelId) {
        self.scoring.modify(channel_id, |i| {
            if i.running_queries > 0 {
                i.running_queries -= 1;
                i.response_count_total += 1;
            }
        });
    }

    pub fn request_finished(&mut self, channel_id: ChannelId) {
        self.scoring.modify(channel_id, |i| {
            if i.running_queries > 0 {
                i.running_queries -= 1;
            }
        });
    }

    pub fn channel(&mut self, mut candidates: Vec<ChannelId>) -> Option<ChannelId> {
        use rand::seq::SliceRandom;
        let mut rng = rsnano_nullable_random::NullableRng::rng();
        candidates.shuffle(&mut rng);

        let limit = self.channel_limit;
        for channel_id in candidates {
            if self.is_banned(channel_id) {
                continue;
            }
            if Self::try_send_message(&mut self.scoring, channel_id, limit) {
                return Some(channel_id);
            }
        }
        None
    }

    fn try_send_message(
        scoring: &mut PeerScoreContainer,
        channel_id: ChannelId,
        channel_limit: usize,
    ) -> bool {
        let mut success = true;
        let modified = scoring.modify(channel_id, |i| {
            if i.running_queries < channel_limit {
                i.running_queries += 1;
                i.request_count_total += 1;
            } else {
                success = false;
            }
        });
        if !modified {
            scoring.insert(PeerScore::new(channel_id));
        }
        success
    }

    pub fn len(&self) -> usize {
        self.scoring.len()
    }

    pub fn decay(&mut self) {
        self.scoring.modify_all(|i| i.decay());
    }

    pub fn clean_up_dead_channels(&mut self, dead_channel_ids: &[ChannelId]) {
        for channel_id in dead_channel_ids {
            self.scoring.remove(*channel_id);
        }
    }

    pub fn banned_channels(&self) -> Vec<ChannelId> {
        self.scoring
            .iter()
            .filter(|i| i.score <= Self::BAN_THRESHOLD)
            .map(|i| i.channel_id)
            .collect()
    }


    pub fn container_info(&self) -> ContainerInfo {
        [("scores", self.len(), 0)].into()
    }
}
