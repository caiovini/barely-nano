use crate::{
    bandwidth_limiter::BandwidthLimiter, channel_stats::ChannelStats,
    utils::into_core_addr, Channel, ChannelDirection,
    ChannelId,
};
use rsnano_core::utils::{TEST_ENDPOINT_1, TEST_ENDPOINT_2};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_nullable_tcp::TcpStream;
use core::fmt::Display;
use core::future::Future;
use alloc::string::ToString;
use alloc::sync::{Arc, Weak};
use kernel_common::async_rt;
use kernel_common::uart;

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
        async_rt::spawn_named(async move {
            loop {
                if channel_w.is_cancelled() {
                    return;
                }

                if let Some(entry) = channel_w.pop().await {
                    kernel_common::uart::putchar(b'|');
                    let mut written = 0;
                    let buffer = &entry.buffer;
                    loop {
                        if channel_w.is_cancelled() {
                            return;
                        }
                        
                        // Wait for writable or cancellation
                        let mut writable_fut = core::pin::pin!(stream_w.writable());
                        let mut cancel_fut = core::pin::pin!(channel_w.cancelled());
                        
                        let ready = core::future::poll_fn(|cx| {
                            if let core::task::Poll::Ready(r) = writable_fut.as_mut().poll(cx) {
                                return core::task::Poll::Ready(Some(r));
                            }
                            if let core::task::Poll::Ready(_) = cancel_fut.as_mut().poll(cx) {
                                return core::task::Poll::Ready(None);
                            }
                            core::task::Poll::Pending
                        }).await;

                        let Some(writable_res) = ready else { return; };
                        kernel_common::uart::putchar(b'>');
                        
                        match writable_res {
                            Ok(()) => match stream_w.try_write(&buffer[written..]) {
                                Ok(n) => {
                                    written += n;
                                    kernel_common::uart::putchar(b'.');
                                    if written >= buffer.len() {
                                        channel_w.set_last_activity(clock_w.now());
                                        break;
                                    }
                                }
                                Err(_) => {
                                    kernel_common::uart::putchar(b'!');
                                    channel_w.write_error();
                                    channel_w.close();
                                    return;
                                }
                            },
                            Err(_) => {
                                kernel_common::uart::putchar(b'!');
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
        }, Some("TcpChannel Write"));

        let channel_l = channel_adapter.clone();
        async_rt::spawn_named(async move { channel_l.ongoing_checkup().await }, Some("TcpChannel Checkup"));
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
            // Sleep to yield executor without pegging CPU
            async_rt::sleep(core::time::Duration::from_secs(1)).await;

            if self.channel.is_closed() {
                break;
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
