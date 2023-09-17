use core::{any::type_name, cmp, iter::from_fn, ops::Deref};

use crate::{
    fs::node::{new_ino, FileAccessContext},
    spin::mutex::Mutex,
};
use alloc::{boxed::Box, collections::BTreeMap, sync::Arc, vec::Vec};
use async_trait::async_trait;
use bitflags::bitflags;
use log::debug;
use x86_64::VirtAddr;

use crate::{
    error::{Error, ErrorKind, Result},
    fs::node::{DirEntry, Directory},
};

use super::{
    memory::{ActiveVirtualMemory, MemoryPermissions, VirtualMemory, VirtualMemoryActivator},
    syscall::args::{EpollEvent, FdNum, FileMode, Pointer, Stat, Whence},
};

pub mod dir;
pub mod epoll;
pub mod eventfd;
pub mod file;
pub mod pipe;
mod std;
pub mod unix_socket;

#[derive(Clone)]
pub struct FileDescriptor {
    ino: u64,
    fd: Arc<dyn OpenFileDescription>,
}

impl FileDescriptor {
    pub fn ino(&self) -> u64 {
        self.ino
    }
}

impl<T> From<T> for FileDescriptor
where
    T: OpenFileDescription,
{
    fn from(value: T) -> Self {
        Self {
            ino: new_ino(),
            fd: Arc::new(value),
        }
    }
}

impl Deref for FileDescriptor {
    type Target = dyn OpenFileDescription;

    fn deref(&self) -> &Self::Target {
        &*self.fd
    }
}

pub struct FileDescriptorTable {
    table: Mutex<BTreeMap<i32, FileDescriptor>>,
}

impl FileDescriptorTable {
    pub const fn empty() -> Self {
        Self {
            table: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn with_standard_io() -> Self {
        let this = Self::empty();

        let stdin = this.insert(std::Stdin).unwrap();
        assert_eq!(stdin.get(), 0);
        let stdout = this.insert(std::Stdout).unwrap();
        assert_eq!(stdout.get(), 1);
        let stderr = this.insert(std::Stderr).unwrap();
        assert_eq!(stderr.get(), 2);

        this
    }

    pub fn insert(&self, fd: impl Into<FileDescriptor>) -> Result<FdNum> {
        self.insert_after(0, fd)
    }

    fn find_free_fd_num(table: &BTreeMap<i32, FileDescriptor>, min: i32) -> Result<i32> {
        const MAX_FD: i32 = i32::MAX;

        let min = cmp::max(0, min);

        let fd_iter = table.keys().copied().skip_while(|i| *i < min);
        let mut counter_iter = min..MAX_FD;

        fd_iter
            .zip(counter_iter.by_ref())
            .find(|(fd, counter)| counter < fd)
            .map(|(_, counter)| counter)
            .or_else(|| counter_iter.next())
            .ok_or_else(|| Error::mfile(()))
    }

    pub fn insert_after(&self, min: i32, fd: impl Into<FileDescriptor>) -> Result<FdNum> {
        let mut guard = self.table.lock();
        let fd_num = Self::find_free_fd_num(&guard, min)?;
        guard.insert(fd_num, fd.into());
        Ok(FdNum::new(fd_num))
    }

    pub fn replace(&self, fd_num: FdNum, fd: impl Into<FileDescriptor>) {
        let mut guard = self.table.lock();
        guard.insert(fd_num.get(), fd.into());
    }

    pub fn get(&self, fd_num: FdNum) -> Result<FileDescriptor> {
        self.table
            .lock()
            .get(&fd_num.get())
            .cloned()
            .ok_or(Error::bad_f(()))
    }

    pub fn close(&self, fd_num: FdNum) -> Result<()> {
        let fd = self
            .table
            .lock()
            .remove(&fd_num.get())
            .ok_or(Error::bad_f(()))?;
        fd.close()
    }

    pub fn nums(&self) -> Vec<(FdNum, FileDescriptor)> {
        let guard = self.table.lock();
        guard
            .iter()
            .map(|(&num, fd)| (FdNum::new(num), fd.clone()))
            .collect()
    }
}

impl Clone for FileDescriptorTable {
    fn clone(&self) -> Self {
        // Copy the table.
        let table = self.table.lock().clone();

        Self {
            table: Mutex::new(table),
        }
    }
}

#[async_trait]
pub trait OpenFileDescription: Send + Sync + 'static {
    fn read(&self, buf: &mut [u8]) -> Result<usize> {
        let _ = buf;
        Err(Error::inval(()))
    }

    fn read_to_user(
        &self,
        vm: &mut ActiveVirtualMemory,
        pointer: Pointer<[u8]>,
        mut len: usize,
    ) -> Result<usize> {
        const MAX_BUFFER_LEN: usize = 8192;
        if len > MAX_BUFFER_LEN {
            len = MAX_BUFFER_LEN;
            debug!("unoptimized read from {} truncated", type_name::<Self>());
        }

        let mut buf = [0; MAX_BUFFER_LEN];
        let buf = &mut buf[..len];

        let count = self.read(buf)?;

        let buf = &buf[..count];
        vm.write_bytes(pointer.get(), buf)?;

        Ok(count)
    }

    fn write(&self, buf: &[u8]) -> Result<usize> {
        let _ = buf;
        Err(Error::inval(()))
    }

    fn write_from_user(
        &self,
        vm: &mut ActiveVirtualMemory,
        pointer: Pointer<[u8]>,
        mut len: usize,
    ) -> Result<usize> {
        const MAX_BUFFER_LEN: usize = 8192;
        if len > MAX_BUFFER_LEN {
            len = MAX_BUFFER_LEN;
            debug!("unoptimized write to {} truncated", type_name::<Self>());
        }

        let mut buf = [0; MAX_BUFFER_LEN];
        let buf = &mut buf[..len];

        vm.read_bytes(pointer.get(), buf)?;

        self.write(buf)
    }

    fn seek(&self, offset: usize, whence: Whence) -> Result<usize> {
        let _ = offset;
        let _ = whence;
        Err(Error::inval(()))
    }

    fn pread(&self, pos: usize, buf: &mut [u8]) -> Result<usize> {
        let _ = pos;
        let _ = buf;
        Err(Error::inval(()))
    }

    fn pwrite(&self, pos: usize, buf: &[u8]) -> Result<usize> {
        let _ = pos;
        let _ = buf;
        Err(Error::inval(()))
    }

    fn close(&self) -> Result<()> {
        Ok(())
    }

    async fn write_all(&self, mut buf: &[u8]) -> Result<()> {
        while !buf.is_empty() {
            let len = do_io(self, Events::WRITE, || self.write(buf)).await?;
            buf = &buf[len..];
        }
        Ok(())
    }

    fn set_mode(&self, mode: FileMode) -> Result<()> {
        let _ = mode;
        Err(Error::io(()))
    }

    fn stat(&self) -> Result<Stat> {
        Err(Error::io(()))
    }

    fn as_dir(&self) -> Result<Arc<dyn Directory>> {
        Err(Error::not_dir(()))
    }

    fn getdents64(&self, capacity: usize, ctx: &FileAccessContext) -> Result<Vec<DirEntry>> {
        let _ = capacity;
        Err(Error::not_dir(()))
    }

    fn mmap(
        &self,
        vm: &mut ActiveVirtualMemory,
        addr: Option<VirtAddr>,
        offset: u64,
        len: u64,
        permissions: MemoryPermissions,
    ) -> Result<VirtAddr> {
        let _ = vm;
        let _ = addr;
        let _ = offset;
        let _ = len;
        let _ = permissions;
        Err(Error::io(()))
    }

    async fn epoll_wait(&self, max_events: usize) -> Result<Vec<EpollEvent>> {
        let _ = max_events;
        Err(Error::inval(()))
    }

    fn epoll_add(&self, fd: FileDescriptor, event: EpollEvent) -> Result<()> {
        let _ = fd;
        let _ = event;
        Err(Error::inval(()))
    }

    fn poll_ready(&self, events: Events) -> Result<Events> {
        let _ = events;
        Err(Error::perm(()))
    }

    async fn ready(&self, events: Events) -> Result<Events> {
        let _ = events;
        Err(Error::perm(()))
    }
}

bitflags! {
    pub struct Events: u8 {
        const READ = 1 << 0;
        const WRITE = 1 << 1;
    }
}

pub async fn do_io<R>(
    fd: &(impl OpenFileDescription + ?Sized),
    events: Events,
    mut callback: impl FnMut() -> Result<R>,
) -> Result<R> {
    loop {
        // Try to execute the closure.
        let res = callback();
        match res {
            Ok(value) => return Ok(value),
            Err(err) if err.kind() == ErrorKind::Again => {
                // Wait for the fd to be ready, then try again.
                fd.ready(events).await?;
            }
            Err(err) => return Err(err),
        }
    }
}

pub async fn do_io_with_vm<R, F>(
    fd: &(impl OpenFileDescription + ?Sized),
    events: Events,
    vm: Arc<VirtualMemory>,
    mut callback: F,
) -> Result<R>
where
    R: Send + 'static,
    F: FnMut(&mut ActiveVirtualMemory) -> Result<R> + Send + 'static,
{
    loop {
        // Try to execute the closure.
        let (res, f) = VirtualMemoryActivator::use_from_async(vm.clone(), move |vm| {
            let res = callback(vm);
            (res, callback)
        })
        .await;
        callback = f;

        match res {
            Ok(value) => return Ok(value),
            Err(err) if err.kind() == ErrorKind::Again => {
                // Wait for the fd to be ready, then try again.
                fd.ready(events).await?;
            }
            Err(err) => return Err(err),
        }
    }
}
