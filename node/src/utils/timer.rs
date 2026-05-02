#[cfg(feature = "output_tracking")]
use alloc::sync::Arc;
use spin::{ Mutex};
use core::time::Duration;

#[cfg(feature = "output_tracking")]
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};

pub trait TimerStrategy: Send {
    fn schedule_with_delay<F>(&self, delay: Duration, cb: F)
    where
        F: 'static + FnMut() + Send;
}

pub struct TimerWrapper;

impl TimerStrategy for TimerWrapper {
    fn schedule_with_delay<F>(&self, _delay: Duration, mut _cb: F)
    where
        F: 'static + FnMut() + Send,
    {
        // Stub: In bare-metal, use a hardware timer
    }
}

pub struct NullTimer;
impl TimerStrategy for NullTimer {
    fn schedule_with_delay<F>(&self, _delay: Duration, _cb: F)
    where
        F: 'static + FnMut() + Send,
    {
    }
}

pub struct Timer<T: TimerStrategy = TimerWrapper> {
    timer: T,
    #[cfg(feature = "output_tracking")]
    listener: OutputListenerMt<TimerEvent>,
}

#[cfg(feature = "output_tracking")]
#[derive(Clone)]
pub struct TimerEvent {
    callback: Arc<dyn Fn() + Send + Sync>,
    pub delay: Duration,
}

#[cfg(feature = "output_tracking")]
impl TimerEvent {
    pub fn execute_callback(&self) {
        (self.callback)()
    }
}

impl Timer<NullTimer> {
    pub fn new_null() -> Self {
        Self::new_with(NullTimer {})
    }
}

impl Timer<TimerWrapper> {
    pub fn new() -> Self {
        Self::new_with(TimerWrapper)
    }
}

impl<T: TimerStrategy> Timer<T> {
    fn new_with(t: T) -> Self {
        Self {
            timer: t,
            #[cfg(feature = "output_tracking")]
            listener: OutputListenerMt::new(),
        }
    }

    #[cfg(feature = "output_tracking")]
    pub fn track(&self) -> Arc<OutputTrackerMt<TimerEvent>> {
        self.listener.track()
    }

    pub fn schedule_with_delay<F>(&self, delay: Duration, cb: F)
    where
        F: 'static + FnMut() + Send,
    {
        #[cfg(feature = "output_tracking")]
        let cb = {
            let option_cb = Arc::new(Mutex::new(Some(cb)));
            let arc_cb: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                let cb = option_cb.lock().take();
                if let Some(mut cb) = cb {
                    cb();
                }
            });

            self.listener.emit(TimerEvent {
                callback: arc_cb.clone(),
                delay,
            });
            move || arc_cb()
        };

        self.timer.schedule_with_delay(delay, cb);
    }
}
