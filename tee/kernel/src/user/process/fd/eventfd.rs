use alloc::sync::Arc;
use bytemuck::pod_read_unaligned;
use log::debug;
use spin::Mutex;

use super::OpenFileDescription;
use crate::{
    async_io::{Blocking, IoNotify},
    error::{Error, Result},
    user::process::syscall::args::EpollEvents,
};

pub struct EventFd {
    state: Mutex<EventFdState>,
}

struct EventFdState {
    counter: u64,
    io_notify: Arc<IoNotify>,
}

impl EventFd {
    pub fn new(initval: u32) -> Self {
        Self {
            state: Mutex::new(EventFdState {
                counter: 0,
                io_notify: Arc::new(IoNotify::new()),
            }),
        }
    }
}

impl OpenFileDescription for EventFd {
    fn read(&self, buf: &mut [u8]) -> Result<Blocking<usize>> {
        if buf.len() != 8 {
            return Err(Error::inval(()));
        }

        let mut state = self.state.lock();
        let value = core::mem::take(&mut state.counter);
        if value == 0 {
            return Ok(Blocking::Blocked(state.io_notify.create_registration()));
        }

        buf.copy_from_slice(&value.to_ne_bytes());
        Ok(Blocking::Some(8))
    }

    fn write(&self, buf: &[u8]) -> Result<usize> {
        if buf.len() != 8 {
            return Err(Error::inval(()));
        }

        let add = pod_read_unaligned::<u64>(buf);

        let mut state = self.state.lock();
        state.counter = state.counter.checked_add(add).expect("TODO");

        state.io_notify.notify();

        Ok(8)
    }

    fn poll(&self, poll_events: EpollEvents) -> Result<EpollEvents> {
        debug!("{poll_events:?}");
        Ok(EpollEvents::empty())
    }
}
