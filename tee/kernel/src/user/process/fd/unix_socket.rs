use log::debug;

use super::OpenFileDescription;
use crate::{async_io::Blocking, error::Result, user::process::syscall::args::EpollEvents};

pub struct UnixSocket {}

impl UnixSocket {
    pub fn new_pair() -> (Self, Self) {
        (Self {}, Self {})
    }
}

impl OpenFileDescription for UnixSocket {
    fn read(&self, buf: &mut [u8]) -> Result<Blocking<usize>> {
        todo!()
    }

    fn write(&self, buf: &[u8]) -> Result<usize> {
        todo!()
    }

    fn poll(&self, poll_events: EpollEvents) -> Result<EpollEvents> {
        debug!("{poll_events:?}");
        Ok(EpollEvents::empty())
    }
}
