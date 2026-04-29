use rsnano_nullable_clock::Timestamp;

/**
 * Token bucket based rate limiting. This is suitable for rate limiting ipc/api calls
 * and network traffic, while allowing short bursts.
 *
 * Tokens are refilled at N tokens per second and there's a bucket capacity to limit
 * bursts.
 *
 * A bucket has low overhead and can be instantiated for various purposes, such as one
 * bucket per session, or one for bandwidth limiting. A token can represent bytes,
 * messages, or the cost of API invocations.
 */
#[derive(Clone)]
pub struct TokenBucket {
    last_refill: Option<Timestamp>,
    current_size: usize,
    max_token_count: usize,

    /** The minimum observed bucket size, from which the largest burst can be derived */
    smallest_size: usize,
    refill_rate: usize,
}

const UNLIMITED: usize = 1_000_000_000;

impl TokenBucket {
    pub fn new(limit: usize) -> Self {
        Self::with_burst_ratio(limit, 1.0)
    }

    pub fn with_burst_ratio(limit: usize, limit_burst_ratio: f64) -> Self {
        Self::with_refill_rate((limit as f64 * limit_burst_ratio) as usize, limit)
    }

    /**
     * Set up a token bucket.
     * @param max_tokens Maximum number of tokens in this bucket, which limits bursts.
     * @param refill_rate Token refill rate, which limits the long term rate (tokens per seconds)
     */
    pub fn with_refill_rate(max_tokens: usize, refill_rate: usize) -> Self {
        let mut result = Self {
            last_refill: None,
            max_token_count: max_tokens,
            refill_rate,
            current_size: 0,
            smallest_size: 0,
        };

        result.reset_with(max_tokens, refill_rate);
        result
    }

    /**
     * Determine if an operation of cost \p tokens_required_a is possible, and deduct from the
     * bucket if that's the case.
     * The default cost is 1 token, but resource intensive operations may request
     * more tokens to be available.
     */
    pub fn try_consume(&mut self, tokens_required: usize, now: Timestamp) -> bool {
        debug_assert!(tokens_required <= UNLIMITED);
        self.refill(now);
        let possible = self.current_size >= tokens_required;
        if possible {
            self.current_size -= tokens_required;
        } else if tokens_required == UNLIMITED {
            self.current_size = 0;
        }

        // Keep track of smallest observed bucket size so burst size can be computed (for tests and stats)
        self.smallest_size = core::cmp::min(self.smallest_size, self.current_size);

        possible || self.refill_rate == UNLIMITED
    }

    pub fn set_limit(&mut self, new_limit: usize) {
        self.max_token_count = new_limit;
        self.refill_rate = new_limit;
    }

    pub fn reset(&mut self) {
        self.reset_with(self.max_token_count, self.refill_rate);
    }

    /** Update the max_token_count and/or refill_rate_a parameters */
    pub fn reset_with(&mut self, mut max_token_count: usize, mut refill_rate: usize) {
        // A token count of 0 indicates unlimited capacity. We use 1e9 as
        // a sentinel, allowing largest burst to still be computed.
        if max_token_count == 0 || refill_rate == 0 {
            refill_rate = UNLIMITED;
            max_token_count = UNLIMITED;
        }
        self.smallest_size = max_token_count;
        self.max_token_count = max_token_count;
        self.current_size = max_token_count;
        self.refill_rate = refill_rate;
        self.last_refill = None;
    }

    /** Returns the largest burst observed */
    #[allow(dead_code)]
    pub fn largest_burst(&self) -> usize {
        self.max_token_count - self.smallest_size
    }

    pub fn size(&self) -> usize {
        self.current_size
    }

    fn refill(&mut self, now: Timestamp) {
        let Some(last_refill) = self.last_refill else {
            self.last_refill = Some(now);
            return;
        };

        let tokens_to_add = (last_refill.elapsed(now).as_nanos() as f64 / 1e9_f64
            * self.refill_rate as f64) as usize;
        // Only update if there are any tokens to add
        if tokens_to_add > 0 {
            self.current_size =
                core::cmp::min(self.current_size + tokens_to_add, self.max_token_count);
            self.last_refill = Some(now);
        }
    }
}
