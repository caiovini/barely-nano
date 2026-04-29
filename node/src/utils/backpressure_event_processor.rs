use rsnano_core::utils::BackpressureReceiver;
use alloc::string::String;
use kernel_common::async_rt;

pub(crate) trait BackpressureEventProcessor<T> {
    fn cool_down(&mut self);
    fn recovered(&mut self);
    fn process(&mut self, event: T);
}

pub(crate) fn spawn_backpressure_processor<T, I>(
    _thread_name: impl Into<String>,
    receiver: BackpressureReceiver<I>,
    mut processor: T,
) where
    I: Send + 'static,
    T: BackpressureEventProcessor<I> + Send + 'static,
{
    async_rt::spawn(async move {
        let mut previous_cooldown_state = false;
        loop {
            // In no_std, we use try_recv and yield if empty to avoid blocking the executor
            if let Ok(event) = receiver.try_recv() {
                // Check if we need to cool down the processing to avoid overwhelming the system
                let current_cooldown = receiver.should_cool_down();

                if current_cooldown != previous_cooldown_state {
                    if current_cooldown {
                        processor.cool_down();
                    } else {
                        processor.recovered();
                    }
                    previous_cooldown_state = current_cooldown;
                }

                processor.process(event);
            } else {
                async_rt::yield_now().await;
            }
        }
    });
}
