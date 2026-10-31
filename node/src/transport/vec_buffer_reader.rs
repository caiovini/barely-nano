use alloc::vec::Vec;
use rsnano_network::AsyncBufferReader;
use core::sync::atomic::{AtomicUsize, Ordering};

pub struct VecBufferReader {
    buffer: Vec<u8>,
    position: AtomicUsize,
}

impl VecBufferReader {
    pub fn new(buffer: Vec<u8>) -> Self {
        Self {
            buffer,
            position: AtomicUsize::new(0),
        }
    }
}

impl AsyncBufferReader for VecBufferReader {
    async fn read(&self, buffer: &mut [u8], count: usize) -> anyhow::Result<()> {
        let pos = self.position.load(Ordering::SeqCst);
        if count > self.buffer.len() - pos {
            bail!("no more data to read");
        }
        buffer[..count].copy_from_slice(&self.buffer[pos..pos + count]);
        self.position.store(pos + count, Ordering::SeqCst);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::{
        future::Future,
        pin::Pin,
        task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
    };

    fn block_on<F: Future>(mut fut: F) -> F::Output {
        fn noop(_: *const ()) {}
        fn clone(p: *const ()) -> RawWaker {
            RawWaker::new(p, &VTABLE)
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
        let waker = unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VTABLE)) };
        let mut cx = Context::from_waker(&waker);
        let mut fut = unsafe { Pin::new_unchecked(&mut fut) };
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(out) => out,
            Poll::Pending => panic!("unexpected Poll::Pending in VecBufferReader"),
        }
    }

    #[test]
    fn empty_vec() {
        let reader = VecBufferReader::new(Vec::new());
        let mut buffer = vec![0u8; 3];
        let result = block_on(reader.read(&mut buffer, 1));
        assert!(result.is_err());
    }

    #[test]
    fn read_one_byte() {
        let reader = VecBufferReader::new(vec![42]);
        let mut buffer = vec![0u8; 1];
        let result = block_on(reader.read(&mut buffer, 1));
        assert!(result.is_ok());
        assert_eq!(buffer[0], 42);
    }

    #[test]
    fn multiple_reads() {
        let reader = VecBufferReader::new(vec![1, 2, 3, 4, 5]);
        let mut buffer = vec![0u8; 2];
        block_on(reader.read(&mut buffer, 1)).unwrap();
        assert_eq!(buffer[0], 1);

        block_on(reader.read(&mut buffer, 2)).unwrap();
        assert_eq!(buffer[0], 2);
        assert_eq!(buffer[1], 3);

        block_on(reader.read(&mut buffer, 2)).unwrap();
        assert_eq!(buffer[0], 4);
        assert_eq!(buffer[1], 5);

        assert!(block_on(reader.read(&mut buffer, 1)).is_err());
    }
}
