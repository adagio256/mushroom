use alloc::vec::Vec;
use log::debug;
use spin::Mutex;

use crate::error::Result;
use crate::user::process::syscall::args::EpollEvent;

use super::{FileDescriptor, OpenFileDescription};

pub struct Epoll {
    interest_list: Mutex<Vec<InterestListEntry>>,
}

impl Epoll {
    pub fn new() -> Self {
        Self {
            interest_list: Mutex::new(Vec::new()),
        }
    }
}

impl OpenFileDescription for Epoll {
    fn epoll_wait(&self, maxevents: usize) -> Result<Vec<EpollEvent>> {
        let guard = self.interest_list.lock();
        let events = guard
            .iter()
            .filter_map(|e| {
                let Ok(events) = e.fd.poll(e.event.events) else {
                    return None;
                };
                if events.is_empty() {
                    return None;
                }
                Some(EpollEvent::new(events, e.event.data))
            })
            .take(maxevents)
            .collect();

        Ok(events)
    }

    fn epoll_add(&self, fd: FileDescriptor, event: EpollEvent) -> Result<()> {
        let mut guard = self.interest_list.lock();

        // Register the file descriptor.
        guard.push(InterestListEntry { fd, event });

        Ok(())
    }
}

struct InterestListEntry {
    fd: FileDescriptor,
    event: EpollEvent,
}
