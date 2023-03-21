use core::{
    convert::Infallible,
    fmt::{self, Display},
    ops::FromResidual,
};

use log::{trace, warn};

use crate::{
    error::{Error, Result},
    per_cpu::PerCpu,
    user::process::{memory::VirtualMemoryActivator, thread::Thread},
};

use super::args::{Ignored, SyscallArg};

#[derive(Debug)]
pub enum SyscallResult {
    Ok(u64),
    Err(Error),
    Yield,
}

impl FromResidual<Result<Infallible, Error>> for SyscallResult {
    fn from_residual(residual: Result<Infallible, Error>) -> Self {
        match residual {
            Ok(value) => match value {},
            Err(err) => Self::Err(err),
        }
    }
}

impl SyscallArg for u32 {
    fn parse(value: u64) -> Result<Self> {
        u32::try_from(value).map_err(|_| Error::Inval)
    }

    fn display(f: &mut dyn fmt::Write, value: u64) -> fmt::Result {
        if let Ok(value) = u32::try_from(value).map_err(|_| Error::Inval) {
            write!(f, "{value}")
        } else {
            write!(f, "{value} (out of bounds)")
        }
    }
}

pub trait Syscall0 {
    const NO: usize;
    const NAME: &'static str;

    fn execute(thread: &mut Thread, vm_activator: &mut VirtualMemoryActivator) -> SyscallResult;

    fn display(f: &mut dyn fmt::Write) -> fmt::Result {
        write!(f, "{}()", Self::NAME)
    }
}

pub trait Syscall1 {
    const NO: usize;
    const NAME: &'static str;

    type Arg0: SyscallArg;

    fn execute(
        thread: &mut Thread,
        vm_activator: &mut VirtualMemoryActivator,
        arg0: Self::Arg0,
    ) -> SyscallResult;

    fn display(f: &mut dyn fmt::Write, arg0: u64) -> fmt::Result {
        write!(f, "{}(", Self::NAME)?;
        <Self::Arg0>::display(f, arg0)?;
        write!(f, ")")
    }
}

pub trait Syscall2 {
    const NO: usize;
    const NAME: &'static str;

    type Arg0: SyscallArg;
    type Arg1: SyscallArg;

    fn execute(
        thread: &mut Thread,
        vm_activator: &mut VirtualMemoryActivator,
        arg0: Self::Arg0,
        arg1: Self::Arg1,
    ) -> SyscallResult;

    fn display(f: &mut dyn fmt::Write, arg0: u64, arg1: u64) -> fmt::Result {
        write!(f, "{}(", Self::NAME)?;
        <Self::Arg0>::display(f, arg0)?;
        write!(f, ", ")?;
        <Self::Arg1>::display(f, arg1)?;
        write!(f, ")")
    }
}

pub trait Syscall3 {
    const NO: usize;
    const NAME: &'static str;

    type Arg0: SyscallArg;
    type Arg1: SyscallArg;
    type Arg2: SyscallArg;

    fn execute(
        thread: &mut Thread,
        vm_activator: &mut VirtualMemoryActivator,
        arg0: Self::Arg0,
        arg1: Self::Arg1,
        arg2: Self::Arg2,
    ) -> SyscallResult;

    fn display(f: &mut dyn fmt::Write, arg0: u64, arg1: u64, arg2: u64) -> fmt::Result {
        write!(f, "{}(", Self::NAME)?;
        <Self::Arg0>::display(f, arg0)?;
        write!(f, ", ")?;
        <Self::Arg1>::display(f, arg1)?;
        write!(f, ", ")?;
        <Self::Arg2>::display(f, arg2)?;
        write!(f, ")")
    }
}

pub trait Syscall4 {
    const NO: usize;
    const NAME: &'static str;

    type Arg0: SyscallArg;
    type Arg1: SyscallArg;
    type Arg2: SyscallArg;
    type Arg3: SyscallArg;

    fn execute(
        thread: &mut Thread,
        vm_activator: &mut VirtualMemoryActivator,
        arg0: Self::Arg0,
        arg1: Self::Arg1,
        arg2: Self::Arg2,
        arg3: Self::Arg3,
    ) -> SyscallResult;

    fn display(f: &mut dyn fmt::Write, arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> fmt::Result {
        write!(f, "{}(", Self::NAME)?;
        <Self::Arg0>::display(f, arg0)?;
        write!(f, ", ")?;
        <Self::Arg1>::display(f, arg1)?;
        write!(f, ", ")?;
        <Self::Arg2>::display(f, arg2)?;
        write!(f, ", ")?;
        <Self::Arg3>::display(f, arg3)?;
        write!(f, ")")
    }
}

pub trait Syscall5 {
    const NO: usize;
    const NAME: &'static str;

    type Arg0: SyscallArg;
    type Arg1: SyscallArg;
    type Arg2: SyscallArg;
    type Arg3: SyscallArg;
    type Arg4: SyscallArg;

    fn execute(
        thread: &mut Thread,
        vm_activator: &mut VirtualMemoryActivator,
        arg0: Self::Arg0,
        arg1: Self::Arg1,
        arg2: Self::Arg2,
        arg3: Self::Arg3,
        arg4: Self::Arg4,
    ) -> SyscallResult;

    fn display(
        f: &mut dyn fmt::Write,
        arg0: u64,
        arg1: u64,
        arg2: u64,
        arg3: u64,
        arg4: u64,
    ) -> fmt::Result {
        write!(f, "{}(", Self::NAME)?;
        <Self::Arg0>::display(f, arg0)?;
        write!(f, ", ")?;
        <Self::Arg1>::display(f, arg1)?;
        write!(f, ", ")?;
        <Self::Arg2>::display(f, arg2)?;
        write!(f, ", ")?;
        <Self::Arg3>::display(f, arg3)?;
        write!(f, ", ")?;
        <Self::Arg4>::display(f, arg4)?;
        write!(f, ")")
    }
}

pub trait Syscall6 {
    const NO: usize;
    const NAME: &'static str;

    type Arg0: SyscallArg;
    type Arg1: SyscallArg;
    type Arg2: SyscallArg;
    type Arg3: SyscallArg;
    type Arg4: SyscallArg;
    type Arg5: SyscallArg;

    fn execute(
        thread: &mut Thread,
        vm_activator: &mut VirtualMemoryActivator,
        arg0: Self::Arg0,
        arg1: Self::Arg1,
        arg2: Self::Arg2,
        arg3: Self::Arg3,
        arg4: Self::Arg4,
        arg5: Self::Arg5,
    ) -> SyscallResult;

    fn display(
        f: &mut dyn fmt::Write,
        arg0: u64,
        arg1: u64,
        arg2: u64,
        arg3: u64,
        arg4: u64,
        arg5: u64,
    ) -> fmt::Result {
        write!(f, "{}(", Self::NAME)?;
        <Self::Arg0>::display(f, arg0)?;
        write!(f, ", ")?;
        <Self::Arg1>::display(f, arg1)?;
        write!(f, ", ")?;
        <Self::Arg2>::display(f, arg2)?;
        write!(f, ", ")?;
        <Self::Arg3>::display(f, arg3)?;
        write!(f, ", ")?;
        <Self::Arg4>::display(f, arg4)?;
        write!(f, ", ")?;
        <Self::Arg5>::display(f, arg5)?;
        write!(f, ")")
    }
}

impl<T> Syscall1 for T
where
    T: Syscall0,
{
    const NO: usize = <T as Syscall0>::NO;
    const NAME: &'static str = <T as Syscall0>::NAME;

    type Arg0 = Ignored;

    fn execute(
        thread: &mut Thread,
        vm_activator: &mut VirtualMemoryActivator,
        _arg0: Self::Arg0,
    ) -> SyscallResult {
        <T as Syscall0>::execute(thread, vm_activator)
    }

    fn display(f: &mut dyn fmt::Write, _arg0: u64) -> fmt::Result {
        <T as Syscall0>::display(f)
    }
}

impl<T> Syscall2 for T
where
    T: Syscall1,
{
    const NO: usize = <T as Syscall1>::NO;
    const NAME: &'static str = <T as Syscall1>::NAME;

    type Arg0 = <T as Syscall1>::Arg0;
    type Arg1 = Ignored;

    fn execute(
        thread: &mut Thread,
        vm_activator: &mut VirtualMemoryActivator,
        arg0: Self::Arg0,
        _arg1: Self::Arg1,
    ) -> SyscallResult {
        <T as Syscall1>::execute(thread, vm_activator, arg0)
    }

    fn display(f: &mut dyn fmt::Write, arg0: u64, _arg1: u64) -> fmt::Result {
        <T as Syscall1>::display(f, arg0)
    }
}

impl<T> Syscall3 for T
where
    T: Syscall2,
{
    const NO: usize = <T as Syscall2>::NO;
    const NAME: &'static str = <T as Syscall2>::NAME;

    type Arg0 = <T as Syscall2>::Arg0;
    type Arg1 = <T as Syscall2>::Arg1;
    type Arg2 = Ignored;

    fn execute(
        thread: &mut Thread,
        vm_activator: &mut VirtualMemoryActivator,
        arg0: Self::Arg0,
        arg1: Self::Arg1,
        _arg2: Self::Arg2,
    ) -> SyscallResult {
        <T as Syscall2>::execute(thread, vm_activator, arg0, arg1)
    }

    fn display(f: &mut dyn fmt::Write, arg0: u64, arg1: u64, _arg2: u64) -> fmt::Result {
        <T as Syscall2>::display(f, arg0, arg1)
    }
}

impl<T> Syscall4 for T
where
    T: Syscall3,
{
    const NO: usize = <T as Syscall3>::NO;
    const NAME: &'static str = <T as Syscall3>::NAME;

    type Arg0 = <T as Syscall3>::Arg0;
    type Arg1 = <T as Syscall3>::Arg1;
    type Arg2 = <T as Syscall3>::Arg2;
    type Arg3 = Ignored;

    fn execute(
        thread: &mut Thread,
        vm_activator: &mut VirtualMemoryActivator,
        arg0: Self::Arg0,
        arg1: Self::Arg1,
        arg2: Self::Arg2,
        _arg3: Self::Arg3,
    ) -> SyscallResult {
        <T as Syscall3>::execute(thread, vm_activator, arg0, arg1, arg2)
    }

    fn display(f: &mut dyn fmt::Write, arg0: u64, arg1: u64, arg2: u64, _arg3: u64) -> fmt::Result {
        <T as Syscall3>::display(f, arg0, arg1, arg2)
    }
}

impl<T> Syscall5 for T
where
    T: Syscall4,
{
    const NO: usize = <T as Syscall4>::NO;
    const NAME: &'static str = <T as Syscall4>::NAME;

    type Arg0 = <T as Syscall4>::Arg0;
    type Arg1 = <T as Syscall4>::Arg1;
    type Arg2 = <T as Syscall4>::Arg2;
    type Arg3 = <T as Syscall4>::Arg3;
    type Arg4 = Ignored;

    fn execute(
        thread: &mut Thread,
        vm_activator: &mut VirtualMemoryActivator,
        arg0: Self::Arg0,
        arg1: Self::Arg1,
        arg2: Self::Arg2,
        arg3: Self::Arg3,
        _arg4: Self::Arg4,
    ) -> SyscallResult {
        <T as Syscall4>::execute(thread, vm_activator, arg0, arg1, arg2, arg3)
    }

    fn display(
        f: &mut dyn fmt::Write,
        arg0: u64,
        arg1: u64,
        arg2: u64,
        arg3: u64,
        _arg4: u64,
    ) -> fmt::Result {
        <T as Syscall4>::display(f, arg0, arg1, arg2, arg3)
    }
}

impl<T> Syscall6 for T
where
    T: Syscall5,
{
    const NO: usize = <T as Syscall5>::NO;
    const NAME: &'static str = <T as Syscall5>::NAME;

    type Arg0 = <T as Syscall5>::Arg0;
    type Arg1 = <T as Syscall5>::Arg1;
    type Arg2 = <T as Syscall5>::Arg2;
    type Arg3 = <T as Syscall5>::Arg3;
    type Arg4 = <T as Syscall5>::Arg4;
    type Arg5 = Ignored;

    fn execute(
        thread: &mut Thread,
        vm_activator: &mut VirtualMemoryActivator,
        arg0: Self::Arg0,
        arg1: Self::Arg1,
        arg2: Self::Arg2,
        arg3: Self::Arg3,
        arg4: Self::Arg4,
        _arg5: Self::Arg5,
    ) -> SyscallResult {
        <T as Syscall5>::execute(thread, vm_activator, arg0, arg1, arg2, arg3, arg4)
    }

    fn display(
        f: &mut dyn fmt::Write,
        arg0: u64,
        arg1: u64,
        arg2: u64,
        arg3: u64,
        arg4: u64,
        _arg5: u64,
    ) -> fmt::Result {
        <T as Syscall5>::display(f, arg0, arg1, arg2, arg3, arg4)
    }
}

const MAX_SYSCALL_HANDLER: usize = 294;

#[derive(Clone, Copy)]
struct SyscallHandler {
    exeute: fn(
        thread: &mut Thread,
        vm_activator: &mut VirtualMemoryActivator,
        arg0: u64,
        arg1: u64,
        arg2: u64,
        arg3: u64,
        arg4: u64,
        arg5: u64,
    ) -> SyscallResult,
    display: fn(
        f: &mut dyn fmt::Write,
        arg0: u64,
        arg1: u64,
        arg2: u64,
        arg3: u64,
        arg4: u64,
        arg5: u64,
    ) -> fmt::Result,
}

impl SyscallHandler {
    const fn new<T>() -> Self
    where
        T: Syscall6,
    {
        Self {
            exeute: |thread: &mut Thread,
                     vm_activator: &mut VirtualMemoryActivator,
                     arg0: u64,
                     arg1: u64,
                     arg2: u64,
                     arg3: u64,
                     arg4: u64,
                     arg5: u64| {
                let arg0 = SyscallArg::parse(arg0)?;
                let arg1 = SyscallArg::parse(arg1)?;
                let arg2 = SyscallArg::parse(arg2)?;
                let arg3 = SyscallArg::parse(arg3)?;
                let arg4 = SyscallArg::parse(arg4)?;
                let arg5 = SyscallArg::parse(arg5)?;
                T::execute(thread, vm_activator, arg0, arg1, arg2, arg3, arg4, arg5)
            },
            display: T::display,
        }
    }
}

pub struct SyscallHandlers {
    handlers: [Option<SyscallHandler>; MAX_SYSCALL_HANDLER],
}

impl SyscallHandlers {
    pub const fn new() -> Self {
        Self {
            handlers: [None; MAX_SYSCALL_HANDLER],
        }
    }

    pub const fn register<T>(&mut self, val: T)
    where
        T: Syscall6,
    {
        self.handlers[T::NO] = Some(SyscallHandler::new::<T>());
        core::mem::forget(val);
    }

    pub fn execute(
        &self,
        thread: &mut Thread,
        vm_activator: &mut VirtualMemoryActivator,
        syscall_no: u64,
        arg0: u64,
        arg1: u64,
        arg2: u64,
        arg3: u64,
        arg4: u64,
        arg5: u64,
    ) -> SyscallResult {
        let syscall_no = usize::try_from(syscall_no).unwrap();
        let handler = self
            .handlers
            .get(syscall_no)
            .copied()
            .flatten()
            .ok_or_else(|| {
                warn!("unsupported syscall: {syscall_no}");
                Error::NoSys
            })?;

        let res = (handler.exeute)(thread, vm_activator, arg0, arg1, arg2, arg3, arg4, arg5);

        let formatted_syscall = FormattedSyscall {
            handler,
            arg0,
            arg1,
            arg2,
            arg3,
            arg4,
            arg5,
        };

        trace!(
            "core={} tid={} @ {formatted_syscall} = {res:?}",
            PerCpu::get().idx,
            thread.tid
        );

        res
    }
}

struct FormattedSyscall {
    handler: SyscallHandler,
    arg0: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
}

impl Display for FormattedSyscall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (self.handler.display)(
            f, self.arg0, self.arg1, self.arg2, self.arg3, self.arg4, self.arg5,
        )
    }
}
