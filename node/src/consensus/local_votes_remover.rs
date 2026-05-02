use super::{ActiveElectionsContainer, LocalVoteHistory};
use rsnano_core::{BlockHash, QualifiedRoot};
use alloc::sync::Arc;
use spin::{ RwLock};

pub(crate) struct LocalVotesRemover {
    pub(crate) vote_history: Arc<LocalVoteHistory>,
    pub(crate) active_elections: Arc<RwLock<ActiveElectionsContainer>>,
}

impl LocalVotesRemover {
    /// Removes votes that were created by this node from an election
    /// if the election winner has changed
    pub fn remove_local_votes(&self, previous_winner: &BlockHash, root: &QualifiedRoot) {
        let votes = self.vote_history.votes(&root.root, &previous_winner, false);

        self.active_elections
            .write()
            .remove_votes(root, votes.iter().map(|i| &i.voter));

        self.vote_history.erase(&root.root);
    }
}
