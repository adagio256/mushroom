use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    arch::x86_64::_rdtsc,
    cell::RefCell,
    fmt::{self, Display},
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
};
use pin_project::pin_project;

use log::{info, trace, warn};

use crate::{
    error::{Error, Result},
    fs::{fd::USE_FROM_ASYNC_COUNTER, node::tmpfs::FILE_COUNTER},
    per_cpu::PerCpu,
    user::process::{
        memory::{VirtualMemoryActivator, FORCE_WRITE_COUNTER, FORCE_WRITE_COUNTER2},
        syscall::{CLONE_COUNTER, DO_IO_COUNTER, FD_GET_COUNTER},
        thread::{Thread, ThreadGuard},
    },
};

use super::args::SyscallArg;

#[derive(Debug, Clone, Copy)]
pub struct SyscallArgs {
    pub abi: Abi,
    pub no: u64,
    pub args: [u64; 6],
}

/// The ABI used during a syscall.
#[derive(Debug, Clone, Copy)]
pub enum Abi {
    I386,
    Amd64,
}

pub type SyscallResult = Result<u64>;

impl SyscallArg for u32 {
    fn parse(value: u64, _abi: Abi) -> Result<Self> {
        Ok(u32::try_from(value)?)
    }

    fn display(
        f: &mut dyn fmt::Write,
        value: u64,
        _abi: Abi,
        _thread: &ThreadGuard<'_>,
        _vm_activator: &mut VirtualMemoryActivator,
    ) -> fmt::Result {
        if let Ok(value) = u32::try_from(value) {
            write!(f, "{value}")
        } else {
            write!(f, "{value} (out of bounds)")
        }
    }
}

pub trait Syscall {
    const NO_I386: Option<usize>;
    const NO_AMD64: Option<usize>;
    const NAME: &'static str;

    #[allow(clippy::too_many_arguments)]
    async fn execute(thread: Arc<Thread>, syscall_args: SyscallArgs) -> SyscallResult;

    #[allow(clippy::too_many_arguments)]
    fn display(
        f: &mut dyn fmt::Write,
        syscall_args: SyscallArgs,
        thread: &ThreadGuard<'_>,
        vm_activator: &mut VirtualMemoryActivator,
    ) -> fmt::Result;
}

const MAX_SYSCALL_I386_HANDLER: usize = 385;
const MAX_SYSCALL_AMD64_HANDLER: usize = 327;

#[derive(Clone, Copy)]
struct SyscallHandler {
    #[allow(clippy::type_complexity)]
    execute: fn(
        thread: Arc<Thread>,
        args: SyscallArgs,
    ) -> Pin<Box<dyn Future<Output = SyscallResult> + Send>>,
    #[allow(clippy::type_complexity)]
    display: fn(
        f: &mut dyn fmt::Write,
        args: SyscallArgs,
        thread: &ThreadGuard<'_>,
        vm_activator: &mut VirtualMemoryActivator,
    ) -> fmt::Result,
}

impl SyscallHandler {
    const fn new<T>() -> Self
    where
        T: Syscall<execute(): Send>,
    {
        Self {
            execute: |thread: Arc<Thread>, args: SyscallArgs| {
                Box::pin(async move { T::execute(thread, args).await })
            },
            display: T::display,
        }
    }
}

pub struct SyscallHandlers {
    i386_handlers: [Option<SyscallHandler>; MAX_SYSCALL_I386_HANDLER],
    amd64_handlers: [Option<SyscallHandler>; MAX_SYSCALL_AMD64_HANDLER],
}

impl SyscallHandlers {
    pub const fn new() -> Self {
        Self {
            i386_handlers: [None; MAX_SYSCALL_I386_HANDLER],
            amd64_handlers: [None; MAX_SYSCALL_AMD64_HANDLER],
        }
    }

    pub const fn register<T>(&mut self, val: T)
    where
        T: Syscall<execute(): Send>,
    {
        if let Some(no) = T::NO_I386 {
            self.i386_handlers[no] = Some(SyscallHandler::new::<T>());
        }
        if let Some(no) = T::NO_AMD64 {
            self.amd64_handlers[no] = Some(SyscallHandler::new::<T>());
        }
        core::mem::forget(val);
    }

    pub async fn execute(&self, thread: Arc<Thread>, args: SyscallArgs) -> SyscallResult {
        let syscall_no = usize::try_from(args.no).unwrap();

        let handlers: &[_] = match args.abi {
            Abi::I386 => &self.i386_handlers,
            Abi::Amd64 => &self.amd64_handlers,
        };

        let handler = handlers.get(syscall_no).copied().flatten().ok_or_else(|| {
            // warn!("unsupported syscall: no={syscall_no}, abi={:?}", args.abi);
            Error::no_sys(())
        })?;

        // Whether the syscall should occur in the debug logs.
        let enable_log = !matches!(syscall_no, 0 | 1 | 3 | 4 | 202 | 228) && thread.tid() != 1;
        // let enable_log = thread.tid() == 5;
        let enable_log = match args.abi {
            Abi::I386 => !matches!(syscall_no, 3 | 4) && false,
            // Abi::Amd64 => !matches!(syscall_no, 0 | 1 | 202 | 228) || thread.tid() == 13,
            Abi::Amd64 => !matches!(syscall_no, 0 | 1 | 202 | 228) || thread.tid() >= 5,
        };
        let enable_log = false;
        // let enable_log = true;

        // let enable_log = thread.tid() == 8001;
        // let enable_log = thread.tid() > 12;

        if enable_log {
            let thread = thread.clone();
            VirtualMemoryActivator::r#do(move |vm_activator| {
                let guard = thread.lock();
                let formatted_syscall = FormattedSyscall {
                    handler,
                    args,
                    thread: &guard,
                    vm_activator: RefCell::new(vm_activator),
                };

                trace!(
                    "core={} tid={} abi={:?} @ {formatted_syscall} = ...",
                    PerCpu::get().idx,
                    guard.tid(),
                    args.abi,
                );
            });
        }

        let (res, duration) = ProfilingFuture::new((handler.execute)(thread.clone(), args)).await;

        static TIMES: [AtomicU64; 512] = [const { AtomicU64::new(0) }; 512];
        TIMES[syscall_no].fetch_add(duration, Ordering::Relaxed);
        static TIMES_NO: [AtomicU64; 512] = [const { AtomicU64::new(0) }; 512];
        TIMES_NO[syscall_no].fetch_add(1, Ordering::Relaxed);

        if syscall_no == 58 {
            // info!("vfork duration={duration}");
        }
        if syscall_no == 59 {
            // info!("execve duration={duration}");
        }

        static COUNTER: AtomicU64 = AtomicU64::new(1);
        if COUNTER.fetch_add(1, Ordering::Relaxed) & 0xf_ffff == 0 && false {
            let mut values = TIMES
                .iter()
                .map(|a| a.load(Ordering::Relaxed))
                .zip(
                    TIMES_NO
                        .iter()
                        .map(|a| a.load(Ordering::Relaxed))
                        .map(|_| 1),
                )
                .map(|(a, b)| a.checked_div(b).unwrap_or_default())
                .zip(0..)
                .collect::<Vec<_>>();
            values.sort();
            values.reverse();

            info!("FILE_COUNTER: {}", FILE_COUNTER.load(Ordering::Relaxed));
            info!("FD_GET_COUNTER: {}", FD_GET_COUNTER.load(Ordering::Relaxed));
            info!("DO_IO_COUNTER: {}", DO_IO_COUNTER.load(Ordering::Relaxed));
            info!(
                "USE_FROM_ASYNC_COUNTER: {}",
                USE_FROM_ASYNC_COUNTER.load(Ordering::Relaxed)
            );
            info!("CLONE_COUNTER: {}", CLONE_COUNTER.load(Ordering::Relaxed));
            info!(
                "FORCE_WRITE_COUNTER: {}",
                FORCE_WRITE_COUNTER.load(Ordering::Relaxed)
            );
            info!(
                "FORCE_WRITE_COUNTER2: {}",
                FORCE_WRITE_COUNTER2.load(Ordering::Relaxed)
            );
            for (value, i) in values.iter() {
                info!("vfork {i:3}: {value}");
            }
            panic!();
        }

        // let enable_log = enable_log || res.is_err();

        if enable_log {
            VirtualMemoryActivator::r#do(move |vm_activator| {
                let guard = thread.lock();
                let formatted_syscall = FormattedSyscall {
                    handler,
                    args,
                    thread: &guard,
                    vm_activator: RefCell::new(vm_activator),
                };

                trace!(
                    "core={} tid={} abi={:?} time={} @ {formatted_syscall} = {res:?}",
                    PerCpu::get().idx,
                    guard.tid(),
                    args.abi,
                    duration
                );
            });
        }

        res
    }
}

struct FormattedSyscall<'a> {
    handler: SyscallHandler,
    args: SyscallArgs,
    thread: &'a ThreadGuard<'a>,
    vm_activator: RefCell<&'a mut VirtualMemoryActivator>,
}

impl Display for FormattedSyscall<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (self.handler.display)(
            f,
            self.args,
            self.thread,
            &mut self.vm_activator.borrow_mut(),
        )
    }
}

#[pin_project]
pub struct ProfilingFuture<F> {
    #[pin]
    future: F,
    duration: u64,
}

impl<F> ProfilingFuture<F> {
    pub fn new(future: F) -> Self {
        Self {
            future,
            duration: 0,
        }
    }
}

impl<F> Future for ProfilingFuture<F>
where
    F: Future,
{
    type Output = (F::Output, u64);

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        let this = self.project();

        let start = unsafe { _rdtsc() };
        let res = this.future.poll(cx);
        let end = unsafe { _rdtsc() };

        *this.duration += end - start;

        res.map(|value| (value, *this.duration))
    }
}
