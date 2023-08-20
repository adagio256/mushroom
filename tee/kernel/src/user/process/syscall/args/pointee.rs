use core::{
    ffi::{c_void, CStr},
    fmt,
    marker::PhantomData,
    mem::{size_of, MaybeUninit},
};

use alloc::ffi::CString;
use bytemuck::{
    bytes_of, bytes_of_mut, cast, checked::try_pod_read_unaligned, CheckedBitPattern, NoUninit,
    Pod, Zeroable,
};
use x86_64::VirtAddr;

use crate::{
    error::{Error, Result},
    fs::path::Path,
    user::process::{
        memory::{ActiveVirtualMemory, VirtualMemoryActivator},
        syscall::cpu_state::Abi,
        thread::{Sigaction, Sigset, Stack, StackFlags, ThreadGuard},
    },
};

use super::{FdNum, Iovec, LongOffset, Offset, Pointer, Stat, Timespec, WStatus};

pub struct Primitive;

pub struct ArchitectureDependent;

pub struct Slice<T>(T);

pub struct Array<T>(T);

pub struct Custom;

pub trait Pointee {
    fn display(
        f: &mut dyn fmt::Write,
        addr: VirtAddr,
        thread: &ThreadGuard,
        vm_activator: &mut VirtualMemoryActivator,
    ) -> fmt::Result {
        let _ = thread;
        let _ = vm_activator;
        write!(f, "{:#x}", addr.as_u64())
    }
}

pub trait ReadablePointee<T>: Pointee + Sized {
    fn read(addr: VirtAddr, vm: &ActiveVirtualMemory, abi: Abi) -> Result<(usize, Self)>;
}

pub trait WritablePointee<T>: Pointee {
    fn write(&self, addr: VirtAddr, vm: &ActiveVirtualMemory, abi: Abi) -> Result<usize>;
}

pub trait PrimitivePointee: Pointee {}

impl<T> ReadablePointee<Primitive> for T
where
    T: PrimitivePointee + CheckedBitPattern,
    [(); size_of::<T>()]: Sized,
{
    fn read(addr: VirtAddr, vm: &ActiveVirtualMemory, _: Abi) -> Result<(usize, Self)> {
        let mut bytes = [0; size_of::<T>()];
        vm.read_bytes(addr, &mut bytes)?;
        let value = try_pod_read_unaligned::<T>(&bytes)?;
        Ok((bytes.len(), value))
    }
}

impl<T> WritablePointee<Primitive> for T
where
    T: PrimitivePointee + NoUninit,
{
    fn write(&self, addr: VirtAddr, vm: &ActiveVirtualMemory, _: Abi) -> Result<usize> {
        vm.write_bytes(addr, bytes_of(self))?;
        Ok(size_of::<Self>())
    }
}

pub trait ArchitectureDependentPointee: Pointee {
    /// Variant of the pointee that is used for the i386 ABI.
    type I386;
    /// Variant of the pointee that is used for the Amd64 ABI.
    type Amd64;
}

impl<T> ReadablePointee<ArchitectureDependent> for T
where
    T: ArchitectureDependentPointee,
    T::I386: CheckedBitPattern + TryInto<Self>,
    <T::I386 as CheckedBitPattern>::Bits: NoUninit,
    T::Amd64: CheckedBitPattern + TryInto<Self>,
    <T::Amd64 as CheckedBitPattern>::Bits: NoUninit,
    Error: From<<T::I386 as TryInto<Self>>::Error> + From<<T::Amd64 as TryInto<Self>>::Error>,
{
    fn read(addr: VirtAddr, vm: &ActiveVirtualMemory, abi: Abi) -> Result<(usize, Self)> {
        match abi {
            Abi::I386 => {
                let mut bits = <T::I386 as CheckedBitPattern>::Bits::zeroed();
                let bytes = bytes_of_mut(&mut bits);
                vm.read_bytes(addr, bytes)?;
                let value = try_pod_read_unaligned::<T::I386>(bytes)?;
                Ok((bytes.len(), value.try_into()?))
            }
            Abi::Amd64 => {
                let mut bits = <T::Amd64 as CheckedBitPattern>::Bits::zeroed();
                let bytes = bytes_of_mut(&mut bits);
                vm.read_bytes(addr, bytes)?;
                let value = try_pod_read_unaligned::<T::Amd64>(bytes)?;
                Ok((bytes.len(), value.try_into()?))
            }
        }
    }
}

impl<T> WritablePointee<ArchitectureDependent> for T
where
    T: ArchitectureDependentPointee,
    T::I386: NoUninit,
    T::Amd64: NoUninit,
    Self: TryInto<T::I386> + TryInto<T::Amd64> + Copy,
    Error: From<<Self as TryInto<T::I386>>::Error> + From<<Self as TryInto<T::Amd64>>::Error>,
{
    fn write(&self, addr: VirtAddr, vm: &ActiveVirtualMemory, abi: Abi) -> Result<usize> {
        match abi {
            Abi::I386 => {
                let value: T::I386 = (*self).try_into()?;
                vm.write_bytes(addr, bytes_of(&value))?;
                Ok(size_of::<T::I386>())
            }
            Abi::Amd64 => {
                let value: T::Amd64 = (*self).try_into()?;
                vm.write_bytes(addr, bytes_of(&value))?;
                Ok(size_of::<T::Amd64>())
            }
        }
    }
}

pub trait AbiAgnosticPointee {}

impl<T> AbiAgnosticPointee for T where T: PrimitivePointee {}
impl<T> AbiAgnosticPointee for [T] where T: AbiAgnosticPointee {}
impl<T, const N: usize> AbiAgnosticPointee for [T; N] where T: AbiAgnosticPointee {}

impl<T> Pointee for [T] where T: Pointee {}

impl<T, P> WritablePointee<Slice<P>> for [T]
where
    T: WritablePointee<P>,
{
    fn write(&self, addr: VirtAddr, vm: &ActiveVirtualMemory, abi: Abi) -> Result<usize> {
        let mut total_len = 0;
        for value in self.iter() {
            let size = value.write(addr + total_len, vm, abi)?;
            total_len += size;
        }
        Ok(total_len)
    }
}

impl<T, const N: usize> Pointee for [T; N] where T: Pointee {}

impl<T, P, const N: usize> ReadablePointee<Array<P>> for [T; N]
where
    T: ReadablePointee<P>,
{
    fn read(addr: VirtAddr, vm: &ActiveVirtualMemory, abi: Abi) -> Result<(usize, Self)> {
        struct PartiallyInitialized<T, const N: usize> {
            initialized: usize,
            arr: [MaybeUninit<T>; N],
        }

        impl<T, const N: usize> PartiallyInitialized<T, N> {
            fn new() -> Self {
                Self {
                    initialized: 0,
                    arr: MaybeUninit::uninit_array(),
                }
            }

            fn push(&mut self, value: T) {
                self.arr[self.initialized].write(value);
                self.initialized += 1;
            }

            fn take(mut self) -> [T; N] {
                assert_eq!(self.initialized, N);
                self.initialized = 0;
                let arr = core::mem::replace(&mut self.arr, MaybeUninit::uninit_array());
                unsafe { MaybeUninit::array_assume_init(arr) }
            }
        }

        impl<T, const N: usize> Drop for PartiallyInitialized<T, N> {
            fn drop(&mut self) {
                for value in self.arr[..self.initialized].iter_mut() {
                    unsafe {
                        value.assume_init_drop();
                    }
                }
            }
        }

        let mut total_len = 0;
        let mut arr = PartiallyInitialized::new();
        for _ in 0..N {
            let (len, value) = ReadablePointee::read(addr + total_len, vm, abi)?;
            total_len += len;
            arr.push(value);
        }
        Ok((total_len, arr.take()))
    }
}

impl<T, P, const N: usize> WritablePointee<Array<P>> for [T; N]
where
    T: WritablePointee<P>,
{
    fn write(&self, addr: VirtAddr, vm: &ActiveVirtualMemory, abi: Abi) -> Result<usize> {
        self.as_slice().write(addr, vm, abi)
    }
}

impl Pointee for u8 {}
impl PrimitivePointee for u8 {}
impl Pointee for u32 {}
impl PrimitivePointee for u32 {}

impl Pointee for CStr {
    fn display(
        f: &mut dyn fmt::Write,
        addr: VirtAddr,
        thread: &ThreadGuard,
        vm_activator: &mut VirtualMemoryActivator,
    ) -> fmt::Result {
        let res = vm_activator.activate(thread.virtual_memory(), |vm| {
            vm.read_cstring(Pointer::from(addr), 128)
        });
        match res {
            Ok(value) => write!(f, "{value:?}"),
            Err(_) => write!(f, "{:#x} (invalid ptr)", addr.as_u64()),
        }
    }
}

impl AbiAgnosticPointee for CStr {}

impl WritablePointee<Custom> for CStr {
    fn write(&self, addr: VirtAddr, vm: &ActiveVirtualMemory, _abi: Abi) -> Result<usize> {
        let bytes = &self.to_bytes_with_nul();
        vm.write_bytes(addr, bytes)?;
        Ok(bytes.len())
    }
}

impl Pointee for CString {
    fn display(
        f: &mut dyn fmt::Write,
        addr: VirtAddr,
        thread: &ThreadGuard,
        vm_activator: &mut VirtualMemoryActivator,
    ) -> fmt::Result {
        let res = vm_activator.activate(thread.virtual_memory(), |vm| {
            vm.read_cstring(Pointer::from(addr), 128)
        });
        match res {
            Ok(value) => write!(f, "{value:?}"),
            Err(_) => write!(f, "{:#x} (invalid ptr)", addr.as_u64()),
        }
    }
}

impl AbiAgnosticPointee for CString {}

impl Pointee for Path {
    fn display(
        f: &mut dyn fmt::Write,
        addr: VirtAddr,
        thread: &ThreadGuard,
        vm_activator: &mut VirtualMemoryActivator,
    ) -> fmt::Result {
        let res = vm_activator.activate(thread.virtual_memory(), |vm| {
            vm.read_cstring(Pointer::from(addr), 128)
        });
        match res {
            Ok(value) => write!(f, "{value:?}"),
            Err(_) => write!(f, "{:#x} (invalid ptr)", addr.as_u64()),
        }
    }
}

impl AbiAgnosticPointee for Path {}

impl ReadablePointee<Custom> for Path {
    fn read(addr: VirtAddr, vm: &ActiveVirtualMemory, _abi: Abi) -> Result<(usize, Self)> {
        const PATH_MAX: usize = 0x1000;
        let pathname = vm.read_cstring(Pointer::from(addr), PATH_MAX)?;
        let len = pathname.to_bytes_with_nul().len();
        let value = Path::new(pathname.into_bytes())?;
        Ok((len, value))
    }
}

impl<T> Pointee for Pointer<T> where T: ?Sized {}

impl<T> ArchitectureDependentPointee for Pointer<T>
where
    T: ?Sized,
{
    type I386 = Pointer32;
    type Amd64 = Pointer64;
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(transparent)]
pub struct Pointer32(u32);

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(transparent)]
pub struct Pointer64(u64);

impl<T> From<Pointer32> for Pointer<T>
where
    T: ?Sized,
{
    fn from(value: Pointer32) -> Self {
        Self {
            value: u64::from(value.0),
            _marker: PhantomData,
        }
    }
}

impl<T> TryFrom<Pointer<T>> for Pointer32
where
    T: ?Sized,
{
    type Error = Error;

    fn try_from(value: Pointer<T>) -> Result<Self> {
        Ok(Self(value.value.try_into()?))
    }
}

impl<T> From<Pointer64> for Pointer<T>
where
    T: ?Sized,
{
    fn from(value: Pointer64) -> Self {
        Self {
            value: value.0,
            _marker: PhantomData,
        }
    }
}

impl<T> TryFrom<Pointer<T>> for Pointer64
where
    T: ?Sized,
{
    type Error = Error;

    fn try_from(value: Pointer<T>) -> Result<Self> {
        Ok(Self(value.value))
    }
}

impl Pointee for FdNum {}

impl PrimitivePointee for FdNum {}

impl Pointee for Sigaction {}

impl ArchitectureDependentPointee for Sigaction {
    type I386 = Sigaction32;
    type Amd64 = Sigaction64;
}

#[derive(Debug, Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct Sigaction32 {
    sa_handler_or_sigaction: u32,
    sa_mask: [u32; 2],
    flags: u32,
    sa_restorer: u32,
}

#[derive(Debug, Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct Sigaction64 {
    sa_handler_or_sigaction: u64,
    sa_mask: Sigset,
    flags: u64,
    sa_restorer: u64,
}

impl TryFrom<Sigaction> for Sigaction32 {
    type Error = Error;

    fn try_from(value: Sigaction) -> Result<Self> {
        Ok(Self {
            sa_handler_or_sigaction: value.sa_handler_or_sigaction.try_into()?,
            sa_mask: cast(value.sa_mask),
            flags: value.flags.try_into()?,
            sa_restorer: value.sa_restorer.try_into()?,
        })
    }
}

impl From<Sigaction> for Sigaction64 {
    fn from(value: Sigaction) -> Self {
        Self {
            sa_handler_or_sigaction: value.sa_handler_or_sigaction,
            sa_mask: value.sa_mask,
            flags: value.flags,
            sa_restorer: value.sa_restorer,
        }
    }
}

impl From<Sigaction32> for Sigaction {
    fn from(value: Sigaction32) -> Self {
        Self {
            sa_handler_or_sigaction: u64::from(value.sa_handler_or_sigaction),
            sa_mask: cast(value.sa_mask),
            flags: u64::from(value.flags),
            sa_restorer: u64::from(value.sa_restorer),
        }
    }
}

impl From<Sigaction64> for Sigaction {
    fn from(value: Sigaction64) -> Self {
        Self {
            sa_handler_or_sigaction: value.sa_handler_or_sigaction,
            sa_mask: value.sa_mask,
            flags: value.flags,
            sa_restorer: value.sa_restorer,
        }
    }
}

impl Pointee for Iovec {}
impl ArchitectureDependentPointee for Iovec {
    type I386 = Iovec32;
    type Amd64 = Iovec64;
}

#[derive(Debug, Clone, Copy, Zeroable, Pod)]
#[repr(C)]
pub struct Iovec32 {
    base: u32,
    len: u32,
}

#[derive(Debug, Clone, Copy, Zeroable, Pod)]
#[repr(C)]
pub struct Iovec64 {
    base: u64,
    len: u64,
}

impl From<Iovec32> for Iovec {
    fn from(value: Iovec32) -> Self {
        Self {
            base: u64::from(value.base),
            len: u64::from(value.len),
        }
    }
}

impl From<Iovec64> for Iovec {
    fn from(value: Iovec64) -> Self {
        Self {
            base: value.base,
            len: value.len,
        }
    }
}

impl Pointee for Timespec {}

impl ArchitectureDependentPointee for Timespec {
    type I386 = Timespec32;
    type Amd64 = Timespec64;
}

#[derive(Debug, Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct Timespec32 {
    pub tv_sec: u32,
    pub tv_nsec: u32,
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct Timespec64 {
    pub tv_sec: u64,
    pub tv_nsec: u64,
}

impl From<Timespec> for Timespec32 {
    fn from(value: Timespec) -> Self {
        Self {
            tv_sec: value.tv_sec,
            tv_nsec: value.tv_nsec,
        }
    }
}

impl From<Timespec> for Timespec64 {
    fn from(value: Timespec) -> Self {
        Self {
            tv_sec: u64::from(value.tv_sec),
            tv_nsec: u64::from(value.tv_nsec),
        }
    }
}

impl From<Timespec32> for Timespec {
    fn from(value: Timespec32) -> Self {
        Self {
            tv_sec: value.tv_sec,
            tv_nsec: value.tv_nsec,
        }
    }
}

impl From<Timespec64> for Timespec {
    fn from(value: Timespec64) -> Self {
        Self {
            tv_sec: u32::try_from(value.tv_sec).unwrap(),
            tv_nsec: u32::try_from(value.tv_nsec).unwrap(),
        }
    }
}

impl Pointee for c_void {}

impl Pointee for Stat {}

impl ArchitectureDependentPointee for Stat {
    type I386 = Stat32;
    type Amd64 = Stat64;
}

#[derive(Clone, Copy, Zeroable, NoUninit)]
#[repr(C)]
pub struct Stat32 {
    pub dev: u32,
    pub ino: u32,
    pub mode: u16,
    pub nlink: u16,
    pub uid: u16,
    pub gid: u16,
    pub rdev: u32,
    pub size: u32,
    pub blksize: u32,
    pub blocks: u32,
    pub atime: Timespec32,
    pub mtime: Timespec32,
    pub ctime: Timespec32,
    pub __unused4: u32,
    pub __unused5: u32,
}

#[derive(Clone, Copy, Zeroable, NoUninit)]
#[repr(C)]
pub struct Stat64 {
    pub dev: u64,
    pub ino: u64,
    pub nlink: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub _pad0: u32,
    pub rdev: u64,
    pub size: i64,
    pub blksize: i64,
    pub blocks: i64,
    pub atime: Timespec64,
    pub mtime: Timespec64,
    pub ctime: Timespec64,
    pub _unused: [i64; 3],
}

impl TryFrom<Stat> for Stat32 {
    type Error = Error;

    fn try_from(value: Stat) -> Result<Self> {
        Ok(Self {
            dev: u32::try_from(value.dev)?,
            ino: u32::try_from(value.ino)?,
            mode: u16::try_from(value.mode.0)?,
            nlink: u16::try_from(value.nlink)?,
            uid: u16::try_from(value.uid)?,
            gid: u16::try_from(value.gid)?,
            rdev: u32::try_from(value.rdev)?,
            size: u32::try_from(value.size)?,
            blksize: u32::try_from(value.blksize)?,
            blocks: u32::try_from(value.blocks)?,
            atime: Timespec32::from(value.atime),
            mtime: Timespec32::from(value.mtime),
            ctime: Timespec32::from(value.ctime),
            __unused4: 0,
            __unused5: 0,
        })
    }
}

impl From<Stat> for Stat64 {
    fn from(value: Stat) -> Self {
        Self {
            dev: value.dev,
            ino: value.ino,
            nlink: value.nlink,
            mode: value.mode.0,
            uid: value.uid,
            gid: value.gid,
            _pad0: 0,
            rdev: value.rdev,
            size: value.size,
            blksize: value.blksize,
            blocks: value.blocks,
            atime: Timespec64::from(value.atime),
            mtime: Timespec64::from(value.mtime),
            ctime: Timespec64::from(value.ctime),
            _unused: [0; 3],
        }
    }
}

impl Pointee for Offset {}

impl ArchitectureDependentPointee for Offset {
    type I386 = Offset32;
    type Amd64 = Offset64;
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(transparent)]
pub struct Offset32(pub i32);

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(transparent)]
pub struct Offset64(pub i64);

impl From<Offset32> for Offset {
    fn from(value: Offset32) -> Self {
        Self(i64::from(value.0))
    }
}

impl From<Offset64> for Offset {
    fn from(value: Offset64) -> Self {
        Self(value.0)
    }
}

impl Pointee for LongOffset {}

impl PrimitivePointee for LongOffset {}

impl Pointee for WStatus {}

impl PrimitivePointee for WStatus {}

impl Pointee for Stack {}

impl ArchitectureDependentPointee for Stack {
    type I386 = Stack32;
    type Amd64 = Stack64;
}

#[derive(Debug, Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct Stack32 {
    pub sp: u32,
    pub flags: StackFlags,
    pub size: u32,
}

#[derive(Debug, Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct Stack64 {
    pub sp: u64,
    pub flags: StackFlags,
    _pad: u32,
    pub size: u64,
}

impl TryFrom<Stack> for Stack32 {
    type Error = Error;

    fn try_from(value: Stack) -> Result<Self> {
        Ok(Self {
            sp: u32::try_from(value.sp)?,
            flags: value.flags,
            size: u32::try_from(value.size)?,
        })
    }
}

impl From<Stack> for Stack64 {
    fn from(value: Stack) -> Self {
        Self {
            sp: value.sp,
            flags: value.flags,
            _pad: 0,
            size: value.size,
        }
    }
}

impl From<Stack32> for Stack {
    fn from(value: Stack32) -> Self {
        Self {
            sp: u64::from(value.sp),
            flags: value.flags,
            size: u64::from(value.size),
        }
    }
}

impl From<Stack64> for Stack {
    fn from(value: Stack64) -> Self {
        Self {
            sp: value.sp,
            flags: value.flags,
            size: value.size,
        }
    }
}

impl Pointee for Sigset {}

impl ArchitectureDependentPointee for Sigset {
    type I386 = Sigset32;
    type Amd64 = Sigset64;
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(transparent)]
pub struct Sigset32([u32; 2]);

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(transparent)]
pub struct Sigset64([u32; 2]);

impl From<Sigset32> for Sigset {
    fn from(value: Sigset32) -> Self {
        Self(cast(value))
    }
}

impl From<Sigset64> for Sigset {
    fn from(value: Sigset64) -> Self {
        Self(cast(value))
    }
}

impl From<Sigset> for Sigset32 {
    fn from(value: Sigset) -> Self {
        cast(value.0)
    }
}

impl From<Sigset> for Sigset64 {
    fn from(value: Sigset) -> Self {
        cast(value.0)
    }
}
