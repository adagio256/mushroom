use core::iter::from_fn;

use alloc::{collections::VecDeque, sync::Arc};
use spin::Mutex;

use super::OpenFileDescription;
use crate::{
    async_io::{Blocking, IoNotify},
    error::Result,
};

struct State {
    buffer: VecDeque<u8>,
    io_notify: Arc<IoNotify>,
}

impl State {
    fn new() -> Self {
        Self {
            buffer: VecDeque::new(),
            io_notify: Arc::new(IoNotify::new()),
        }
    }
}

pub struct ReadHalf {
    state: Arc<Mutex<State>>,
}

impl OpenFileDescription for ReadHalf {
    fn read(&self, buf: &mut [u8]) -> Result<Blocking<usize>> {
        let mut guard = self.state.lock();

        if guard.buffer.is_empty() {
            if Arc::strong_count(&self.state) == 1 {
                return Ok(Blocking::Some(0));
            }

            return Ok(Blocking::Blocked(guard.io_notify.create_registration()));
        }

        let mut read = 0;
        for (dest, src) in buf.iter_mut().zip(from_fn(|| guard.buffer.pop_front())) {
            *dest = src;
            read += 1;
        }

        Ok(Blocking::Some(read))
    }
}

pub struct WriteHalf {
    state: Arc<Mutex<State>>,
}

impl OpenFileDescription for WriteHalf {
    fn write(&self, buf: &[u8]) -> Result<usize> {
        let mut guard = self.state.lock();
        guard.buffer.extend(buf.iter().copied());

        guard.io_notify.notify();

        Ok(buf.len())
    }
}

pub fn new() -> (ReadHalf, WriteHalf) {
    let state = Arc::new(Mutex::new(State::new()));

    (
        ReadHalf {
            state: state.clone(),
        },
        WriteHalf { state },
    )
}
