use crate::{
    bandwidth_limiter::BandwidthLimiter, channel_stats::ChannelStats,
    utils::into_core_addr, Channel, ChannelDirection,
    ChannelId,
};
use rsnano_core::utils::{TEST_ENDPOINT_1, TEST_ENDPOINT_2};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_nullable_tcp::TcpStream;
use core::{
    fmt::Display,
    time::Duration,
    future::poll_fn,
    task::Poll,
};
use alloc::sync::{Arc, Weak};
use alloc::string::ToString;
use kernel_common::async_rt;

/// Connects a Channel with a TcpStream
pub struct TcpChannelAdapter {
    pub channel: Arc<Channel>,
    stream: Weak<TcpStream>,
    clock: Arc<SteadyClock>,
}

impl TcpChannelAdapter {
    fn new(channel: Arc<Channel>, stream: Weak<TcpStream>, clock: Arc<SteadyClock>) -> Self {
        Self {
            channel,
            stream,
            clock,
        }
    }

    pub fn new_null() -> Self {
        Self::new_null_with_id(42)
    }

    pub fn new_null_with_id(id: impl Into<ChannelId>) -> Self {
        let channel_id = id.into();
        let channel = Self::new(
            Arc::new(Channel::new(
                channel_id,
                into_core_addr(TEST_ENDPOINT_1),
                into_core_addr(TEST_ENDPOINT_2),
                ChannelDirection::Outbound,
                u8::MAX,
                Timestamp::new_test_instance(),
                Arc::new(BandwidthLimiter::default()),
                Arc::new(ChannelStats::default()),
            )),
            Arc::downgrade(&Arc::new(TcpStream::new_null())),
            Arc::new(SteadyClock::new_null()),
        );
        channel
    }

    pub fn create(
        channel: Arc<Channel>,
        stream: TcpStream,
        clock: Arc<SteadyClock>,
    ) -> Arc<Self> {
        let stream = Arc::new(stream);
        let channel_adapter = Arc::new(Self::new(channel.clone(), Arc::downgrade(&stream), clock.clone()));

        // process write queue:
        let channel_w = channel.clone();
        let stream_w = stream.clone();
        let clock_w = clock.clone();
        async_rt::spawn(async move {
            loop {
                if channel_w.is_cancelled() {
                    return;
                }

                if let Some(entry) = channel_w.pop().await {
                    let mut written = 0;
                    let buffer = &entry.buffer;
                    loop {
                        if channel_w.is_cancelled() {
                            return;
                        }
                        
                        match stream_w.writable().await {
                            Ok(()) => match stream_w.try_write(&buffer[written..]) {
                                Ok(n) => {
                                    written += n;
                                    if written >= buffer.len() {
                                        channel_w.set_last_activity(clock_w.now());
                                        break;
                                    }
                                }
                                Err(_) => {
                                    channel_w.write_error();
                                    channel_w.close();
                                    return;
                                }
                            },
                            Err(_) => {
                                channel_w.write_error();
                                channel_w.close();
                                return;
                            }
                        }
                    }
                } else {
                    break;
                }
            }
            channel_w.close();
        });

        let channel_l = channel_adapter.clone();
        async_rt::spawn(async move { channel_l.ongoing_checkup().await });
        channel_adapter
    }

    pub async fn readable(&self) -> anyhow::Result<()> {
        if self.channel.is_closed() {
            return Err(anyhow::anyhow!("Tried to read from a closed TcpStream"));
        }

        let Some(stream) = self.stream.upgrade() else {
            return Err(anyhow::anyhow!("TCP stream dropped"));
        };

        stream.readable().await.map_err(|e| anyhow::anyhow!(e))
    }

    pub fn try_read(&self, buffer: &mut [u8]) -> anyhow::Result<usize> {
        let Some(stream) = self.stream.upgrade() else {
            return Err(anyhow::anyhow!("TCP stream dropped"));
        };

        match stream.try_read(buffer) {
            Ok(0) => {
                self.channel.read_failed();
                Err(anyhow::anyhow!("remote side closed the channel"))
            }
            Ok(n) => {
                self.channel.read_succeeded(n, self.clock.now());
                Ok(n)
            }
            Err(e) => {
                if e.to_string().contains("Would block") {
                    Ok(0)
                } else {
                    self.channel.read_failed();
                    Err(e)
                }
            }
        }
    }

    async fn ongoing_checkup(&self) {
        loop {
            // Simple yielding wait
            for _ in 0..5 {
                async_rt::yield_now().await;
                for _ in 0..100 { core::hint::spin_loop(); }
            }

            let now = self.clock.now();
            let timed_out = self.channel.check_timeout(now);
            if timed_out {
                break;
            }
        }
    }
}

impl Display for TcpChannelAdapter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&self.channel.peer_addr(), f)
    }
}

impl Drop for TcpChannelAdapter {
    fn drop(&mut self) {
        self.channel.close();
    }
}
