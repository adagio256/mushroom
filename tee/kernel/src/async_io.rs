use alloc::{boxed::Box, collections::VecDeque, sync::Arc, vec, vec::Vec};
use spin::Mutex;

use crate::user::process::{memory::VirtualMemoryActivator, thread::Thread};

pub enum Blocking<T> {
    Some(T),
    Blocked(IoNotifyRegistration),
}

pub fn try_io<T, E>(
    res: Result<Blocking<T>, E>,
    f: impl FnOnce(Result<T, E>),
) -> Result<(), RequiresRetry> {
    match res {
        Ok(Blocking::Some(value)) => {
            f(Ok(value));
            Ok(())
        }
        Err(err) => {
            f(Err(err));
            Ok(())
        }
        Ok(Blocking::Blocked(_)) => Err(RequiresRetry),
    }
}

impl<T> Blocking<T> {
    pub fn try_io<E>(res: Result<Blocking<T>, E>) -> Blocking<Result<T, E>> {
        match res {
            Ok(_) => todo!(),
            Err(_) => todo!(),
        }
    }
}

pub struct IoNotify {
    state: Mutex<IoNotifyState>,
}

impl IoNotify {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(IoNotifyState {
                counter: 0,
                callbacks: Vec::new(),
            }),
        }
    }

    pub fn notify(self: &Arc<Self>) {
        let mut state = self.state.lock();

        // Increase the counter.
        state.counter += 1;

        // Take all callbacks and record the counter.
        let counter = state.counter;
        let callbacks = core::mem::take(&mut state.callbacks);
        drop(state);

        PENDING_IO_NOTIFICATIONS
            .lock()
            .push_back(PendingIoNotifications {
                notify: self.clone(),
                counter,
                callbacks,
            });
    }

    pub fn create_registration(self: &Arc<Self>) -> IoNotifyRegistration {
        IoNotifyRegistration {
            counter: self.state.lock().counter,
            notify: self.clone(),
        }
    }
}

struct IoNotifyState {
    counter: u64,
    callbacks: Vec<Box<dyn FnMut(&mut VirtualMemoryActivator) -> Result<(), RequiresRetry> + Send>>,
}

pub struct IoNotifyRegistration {
    counter: u64,
    notify: Arc<IoNotify>,
}

impl IoNotifyRegistration {
    pub fn register(
        self,
        mut f: impl FnMut(&mut VirtualMemoryActivator) -> Result<(), RequiresRetry> + Send + 'static,
    ) {
        let boxed = Box::new(f);

        let mut state = self.notify.state.lock();
        if state.counter == self.counter {
            state.callbacks.push(boxed);
            return;
        }
        let counter = state.counter;
        drop(state);

        PENDING_IO_NOTIFICATIONS
            .lock()
            .push_back(PendingIoNotifications {
                notify: self.notify,
                counter,
                callbacks: vec![boxed],
            })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RequiresRetry;

struct PendingIoNotifications {
    notify: Arc<IoNotify>,
    counter: u64,
    callbacks: Vec<Box<dyn FnMut(&mut VirtualMemoryActivator) -> Result<(), RequiresRetry> + Send>>,
}

impl PendingIoNotifications {
    pub fn run(mut self, vm_activator: &mut VirtualMemoryActivator) {
        loop {
            // Execute all callbacks. Keep those that need to be retried.
            self.callbacks.retain_mut(|c| (*c)(vm_activator).is_err());
            // Return if no callbacks need to be retried.
            if self.callbacks.is_empty() {
                return;
            }

            let mut state = self.notify.state.lock();
            // If the counter has not since been increased ...
            if state.counter == self.counter {
                // ... put them back into the vector.
                state.callbacks.append(&mut self.callbacks);
                break;
            }

            // Otherwise try again with the new counter.
            self.counter = state.counter;
        }
    }
}

static PENDING_IO_NOTIFICATIONS: Mutex<VecDeque<PendingIoNotifications>> =
    Mutex::new(VecDeque::new());

/// Returns true if some pending io notifications were run.
pub fn run_pending_notifications(vm_activator: &mut VirtualMemoryActivator) -> bool {
    let Some(pending_io_notifications) = PENDING_IO_NOTIFICATIONS.lock().pop_front() else {
        return false;
    };

    pending_io_notifications.run(vm_activator);

    true
}
