use core::{
    arch::x86_64::_rdtsc,
    cmp,
    iter::from_fn,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{
    dir_impls,
    fs::fd::{
        dir::{open_dir, Directory},
        file::{open_file, File},
        FileDescriptor,
    },
    spin::mutex::Mutex,
    time::now,
    user::process::syscall::args::OpenFlags,
};
use alloc::{
    borrow::{Cow, ToOwned},
    collections::{btree_map::Entry, BTreeMap},
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use log::info;

use super::{
    create_file, lookup_node_with_parent, new_ino, DirEntry, DirEntryName, DynINode,
    FileAccessContext, FileSnapshot, INode,
};
use crate::{
    error::{Error, Result},
    fs::path::{FileName, Path},
    user::process::{
        memory::ActiveVirtualMemory,
        syscall::args::{FileMode, FileType, FileTypeAndMode, Pointer, Stat, Timespec},
    },
};

pub struct TmpFsDir {
    ino: u64,
    this: Weak<Self>,
    parent: Weak<dyn INode>,
    internal: Mutex<DevTmpFsDirInternal>,
}

struct DevTmpFsDirInternal {
    mode: FileMode,
    items: BTreeMap<FileName<'static>, DynINode>,
    atime: Timespec,
    mtime: Timespec,
    ctime: Timespec,
}

impl TmpFsDir {
    pub fn root(mode: FileMode) -> Arc<Self> {
        let now = now();

        Arc::new_cyclic(|this_weak| Self {
            ino: new_ino(),
            this: this_weak.clone(),
            parent: this_weak.clone(),
            internal: Mutex::new(DevTmpFsDirInternal {
                mode,
                items: BTreeMap::new(),
                atime: now,
                mtime: now,
                ctime: now,
            }),
        })
    }

    pub fn new(parent: Weak<dyn INode>, mode: FileMode) -> Arc<Self> {
        let now = now();

        Arc::new_cyclic(|this_weak| Self {
            ino: new_ino(),
            this: this_weak.clone(),
            parent,
            internal: Mutex::new(DevTmpFsDirInternal {
                mode,
                items: BTreeMap::new(),
                atime: now,
                mtime: now,
                ctime: now,
            }),
        })
    }
}

impl INode for TmpFsDir {
    dir_impls!();

    fn stat(&self) -> Stat {
        let guard = self.internal.lock();
        let mode = FileTypeAndMode::new(FileType::Dir, guard.mode);
        // FIXME: Fill in more values.
        Stat {
            dev: 0,
            ino: self.ino,
            nlink: 1,
            mode,
            uid: 0,
            gid: 0,
            rdev: 0,
            size: 0,
            blksize: 0,
            blocks: 0,
            atime: guard.atime,
            mtime: guard.mtime,
            ctime: guard.ctime,
        }
    }

    fn open(&self, flags: OpenFlags) -> Result<FileDescriptor> {
        open_dir(self.this.upgrade().unwrap(), flags)
    }

    fn set_mode(&self, mode: FileMode) {
        self.internal.lock().mode = mode;
    }

    fn mount(&self, file_name: FileName<'static>, node: DynINode) -> Result<()> {
        self.internal.lock().items.insert(file_name.clone(), node);
        Ok(())
    }
}

impl Directory for TmpFsDir {
    fn parent(&self) -> Result<DynINode> {
        self.parent
            .clone()
            .upgrade()
            .ok_or_else(|| Error::no_ent(()))
    }

    fn get_node(&self, path_segment: &FileName, _ctx: &FileAccessContext) -> Result<DynINode> {
        self.internal
            .lock()
            .items
            .get(path_segment)
            .cloned()
            .ok_or(Error::no_ent(()))
    }

    fn create_dir(&self, file_name: FileName<'static>, mode: FileMode) -> Result<DynINode> {
        let mut guard = self.internal.lock();
        let entry = guard.items.entry(file_name);
        match entry {
            Entry::Vacant(entry) => {
                let dir = TmpFsDir::new(self.this.clone(), mode);
                entry.insert(dir.clone());
                Ok(dir)
            }
            Entry::Occupied(_) => Err(Error::exist(())),
        }
    }

    fn create_file(
        &self,
        path_segment: FileName<'static>,
        mode: FileMode,
        create_new: bool,
        ctx: &mut FileAccessContext,
    ) -> Result<DynINode> {
        let mut guard = self.internal.lock();
        let entry = guard.items.entry(path_segment);
        match entry {
            Entry::Vacant(entry) => {
                let node = TmpFsFile::new(mode, &[]);
                entry.insert(node.clone());
                Ok(node)
            }
            Entry::Occupied(entry) => {
                if create_new {
                    return Err(Error::exist(()));
                }
                let entry = entry.get();
                if entry.ty() == FileType::Link {
                    let this = self.this.upgrade().unwrap();
                    let link = entry.read_link()?;
                    return create_file(this, &link, mode, create_new, ctx);
                }
                if entry.ty() != FileType::File {
                    return Err(Error::exist(()));
                }
                Ok(entry.clone())
            }
        }
    }

    fn create_link(
        &self,
        file_name: FileName<'static>,
        target: Path,
        create_new: bool,
    ) -> Result<DynINode> {
        let mut guard = self.internal.lock();
        let entry = guard.items.entry(file_name);
        match entry {
            Entry::Vacant(entry) => {
                let link = Arc::new(TmpFsSymlink {
                    ino: new_ino(),
                    target,
                });
                entry.insert(link.clone());
                Ok(link)
            }
            Entry::Occupied(mut entry) => {
                if create_new {
                    return Err(Error::exist(()));
                }
                let link = Arc::new(TmpFsSymlink {
                    ino: new_ino(),
                    target,
                });
                entry.insert(link.clone());
                Ok(link)
            }
        }
    }

    fn hard_link(&self, file_name: FileName<'static>, node: DynINode) -> Result<()> {
        self.internal.lock().items.insert(file_name.clone(), node);
        Ok(())
    }

    fn list_entries(&self, _ctx: &mut FileAccessContext) -> Vec<DirEntry> {
        let guard = self.internal.lock();

        let mut entries = Vec::with_capacity(2 + guard.items.len());
        entries.push(DirEntry {
            ino: 0,
            ty: FileType::Dir,
            name: DirEntryName::Dot,
        });
        entries.push(DirEntry {
            ino: 0,
            ty: FileType::Dir,
            name: DirEntryName::DotDot,
        });
        for (name, node) in guard.items.iter() {
            let stat = node.stat();
            entries.push(DirEntry {
                ino: stat.ino,
                ty: stat.mode.ty(),
                name: DirEntryName::from(name.clone()),
            })
        }
        entries
    }

    fn delete_non_dir(&self, file_name: FileName<'static>) -> Result<()> {
        let mut guard = self.internal.lock();
        let node = guard.items.entry(file_name);
        let Entry::Occupied(entry) = node else {
            return Err(Error::no_ent(()));
        };
        if entry.get().ty() == FileType::Dir {
            return Err(Error::is_dir(()));
        }
        entry.remove();
        Ok(())
    }

    fn delete_dir(&self, file_name: FileName<'static>) -> Result<()> {
        let mut guard = self.internal.lock();
        let node = guard.items.entry(file_name);
        let Entry::Occupied(entry) = node else {
            return Err(Error::no_ent(()));
        };
        if entry.get().ty() != FileType::Dir {
            return Err(Error::is_dir(()));
        }
        entry.remove();
        Ok(())
    }
}

pub struct TmpFsFile {
    ino: u64,
    this: Weak<Self>,
    internal: Mutex<TmpFsFileInternal>,
}

struct TmpFsFileInternal {
    contents: Buffer,
    mode: FileMode,
    atime: Timespec,
    mtime: Timespec,
    ctime: Timespec,
}

impl TmpFsFile {
    pub fn new(mode: FileMode, content: &'static [u8]) -> Arc<Self> {
        let now = now();

        Arc::new_cyclic(|this| Self {
            this: this.clone(),
            ino: new_ino(),
            internal: Mutex::new(TmpFsFileInternal {
                contents: Buffer::new(content),
                mode,
                atime: now,
                mtime: now,
                ctime: now,
            }),
        })
    }
}

impl INode for TmpFsFile {
    fn stat(&self) -> Stat {
        let guard = self.internal.lock();
        let mode = FileTypeAndMode::new(FileType::File, guard.mode);
        let size = guard.contents.len() as i64;

        // FIXME: Fill in more values.
        Stat {
            dev: 0,
            ino: self.ino,
            nlink: 1,
            mode,
            uid: 0,
            gid: 0,
            rdev: 0,
            size,
            blksize: 0,
            blocks: 0,
            atime: guard.atime,
            mtime: guard.mtime,
            ctime: guard.ctime,
        }
    }

    fn open(&self, flags: OpenFlags) -> Result<FileDescriptor> {
        open_file(self.this.upgrade().unwrap(), flags)
    }

    fn set_mode(&self, mode: FileMode) {
        self.internal.lock().mode = mode;
    }

    fn read_snapshot(&self) -> Result<FileSnapshot> {
        let mut guard = self.internal.lock();
        Ok(guard.contents.to_snapshot())
    }
}

pub static FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

impl File for TmpFsFile {
    fn read(&self, offset: usize, mut buf: &mut [u8]) -> Result<usize> {
        let guard = self.internal.lock();

        if offset > guard.contents.len() {
            return Err(Error::inval(()));
        }

        let mut read = 0;
        for buffer in guard.contents.read(offset, buf.len()) {
            read += buffer.len();

            let (chunk, rest) = buf.split_at_mut(buffer.len());
            chunk.copy_from_slice(buffer);
            buf = rest;
        }

        Ok(read)
    }

    fn read_to_user(
        &self,
        offset: usize,
        vm: &mut ActiveVirtualMemory,
        mut pointer: Pointer<[u8]>,
        len: usize,
    ) -> Result<usize> {
        let guard = self.internal.lock();

        if offset > guard.contents.len() {
            return Err(Error::inval(()));
        }

        // info!("reading");

        let mut read = 0;
        for buffer in guard.contents.read(offset, len) {
            vm.write_bytes(pointer.get(), buffer)?;

            read += buffer.len();
            pointer = pointer.bytes_offset(buffer.len());
        }

        // info!("done reading");

        Ok(read)
    }

    fn write(&self, offset: usize, mut buf: &[u8]) -> Result<usize> {
        let mut guard = self.internal.lock();

        // info!("writing");

        let len = buf.len();

        guard.contents.grow(offset + buf.len());
        for buffer in guard.contents.write(offset, buf.len()) {
            let (chunk, next) = buf.split_at(buffer.len());
            buffer.copy_from_slice(chunk);
            buf = next;
        }

        // info!("done writing");

        Ok(len)
    }

    fn write_from_user(
        &self,
        offset: usize,
        vm: &mut ActiveVirtualMemory,
        mut pointer: Pointer<[u8]>,
        len: usize,
    ) -> Result<usize> {
        let mut guard = self.internal.lock();

        // info!("writing");

        let start = unsafe { _rdtsc() };

        guard.contents.grow(offset + len);
        for buffer in guard.contents.write(offset, len) {
            vm.read_bytes(pointer.get(), buffer)?;
            pointer = pointer.bytes_offset(buffer.len());
        }

        let end = unsafe { _rdtsc() };
        FILE_COUNTER.fetch_add(end - start, Ordering::Relaxed);

        // info!("done writing");

        Ok(len)
    }

    fn append(&self, mut buf: &[u8]) -> Result<usize> {
        let mut guard = self.internal.lock();

        let offset = guard.contents.len();

        let len = buf.len();

        guard.contents.grow(offset + buf.len());
        for buffer in guard.contents.write(offset, buf.len()) {
            let (chunk, next) = buf.split_at(buffer.len());
            buffer.copy_from_slice(chunk);
            buf = next;
        }

        Ok(len)
    }

    fn append_from_user(
        &self,
        vm: &mut ActiveVirtualMemory,
        mut pointer: Pointer<[u8]>,
        len: usize,
    ) -> Result<usize> {
        let mut guard = self.internal.lock();

        let start = unsafe { _rdtsc() };

        let offset = guard.contents.len();
        guard.contents.grow(offset + len);
        for buffer in guard.contents.write(offset, len) {
            vm.read_bytes(pointer.get(), buffer)?;
            pointer = pointer.bytes_offset(buffer.len());
        }

        let end = unsafe { _rdtsc() };
        FILE_COUNTER.fetch_add(end - start, Ordering::Relaxed);

        Ok(len)
    }

    fn truncate(&self, len: u64) -> Result<()> {
        let mut guard = self.internal.lock();
        guard.contents.truncate(usize::try_from(len).unwrap());
        Ok(())
    }
}

#[derive(Clone)]
pub struct TmpFsSymlink {
    ino: u64,
    target: Path,
}

impl INode for TmpFsSymlink {
    fn stat(&self) -> Stat {
        Stat {
            dev: 0,
            ino: self.ino,
            nlink: 1,
            mode: FileTypeAndMode::new(FileType::Link, FileMode::ALL),
            uid: 0,
            gid: 0,
            rdev: 0,
            size: 0,
            blksize: 0,
            blocks: 0,
            atime: Timespec::ZERO,
            mtime: Timespec::ZERO,
            ctime: Timespec::ZERO,
        }
    }

    fn open(&self, _flags: OpenFlags) -> Result<FileDescriptor> {
        Err(Error::r#loop(()))
    }

    fn set_mode(&self, _mode: FileMode) {
        todo!()
    }

    fn read_link(&self) -> Result<Path> {
        Ok(self.target.clone())
    }

    fn try_resolve_link(
        &self,
        start_dir: DynINode,
        ctx: &mut FileAccessContext,
    ) -> Result<Option<(DynINode, DynINode)>> {
        ctx.follow_symlink()?;
        lookup_node_with_parent(start_dir, &self.target, ctx).map(Some)
    }
}

struct Buffer {
    contents: Vec<Vec<u8>>,
    snapshot: Option<FileSnapshot>,
}

impl Buffer {
    pub fn new(content: &[u8]) -> Self {
        let mut this = Self {
            contents: Vec::new(),
            snapshot: None,
        };
        if !content.is_empty() {
            this.contents.push(content.to_owned());
        }
        this
    }

    pub fn len(&self) -> usize {
        self.contents.iter().map(Vec::len).sum()
    }

    pub fn to_snapshot(&mut self) -> FileSnapshot {
        if let Some(snapshot) = self.snapshot.as_ref() {
            return snapshot.clone();
        }

        info!("creating snapshot");

        let len = self.len();
        let mut content = Vec::with_capacity(len);
        for buffer in self.read(0, len) {
            content.extend_from_slice(buffer);
        }
        let snapshot = FileSnapshot(Arc::new(Cow::Owned(content)));
        self.snapshot = Some(snapshot.clone());

        info!("done creating snapshot len={len}");

        snapshot
    }

    pub fn read(&self, mut offset: usize, mut len: usize) -> impl Iterator<Item = &[u8]> {
        let mut iter = self.contents.iter();
        from_fn(move || loop {
            let mut next = &**iter.next()?;

            if next.len() <= offset {
                offset -= next.len();
                continue;
            } else {
                next = &next[offset..];
                offset = 0;
            }

            if next.is_empty() {
                continue;
            }

            let chunk_len = cmp::min(next.len(), len);
            len -= chunk_len;
            return Some(&next[..chunk_len]);
        })
    }

    pub fn write(&mut self, mut offset: usize, mut len: usize) -> impl Iterator<Item = &mut [u8]> {
        self.snapshot = None;

        assert!(self.len() >= offset + len);

        let mut iter = self.contents.iter_mut();
        from_fn(move || loop {
            let mut next = &mut **iter.next()?;

            if next.len() <= offset {
                offset -= next.len();
                continue;
            } else {
                next = &mut next[offset..];
                offset = 0;
            }

            if next.is_empty() {
                continue;
            }

            let chunk_len = cmp::min(next.len(), len);
            len -= chunk_len;
            return Some(&mut next[..chunk_len]);
        })
    }

    pub fn grow(&mut self, to: usize) {
        let diff = to.saturating_sub(self.len());
        if diff == 0 {
            return;
        }

        self.snapshot = None;

        let mut diff = diff;
        if let Some(last) = self.contents.last_mut() {
            last.resize(to, 0);
            return;

            let available = last.capacity() - last.len();
            let b = cmp::min(available, diff);
            last.resize(last.len() + b, 0);
            diff -= b;

            if diff == 0 {
                assert!(self.len() >= to);

                return;
            }
        }

        if diff == 1 {
            // info!("small buf");
        }

        let capacity = cmp::max(diff, 1024).next_power_of_two();
        let capacity = cmp::max(diff, self.len()).next_power_of_two();
        let mut vec = Vec::with_capacity(capacity);
        vec.resize(diff, 0);

        self.contents.push(vec);

        assert!(self.len() >= to);
    }

    pub fn truncate(&mut self, to: usize) {
        if self.len() == to {
            return;
        }

        self.snapshot = None;

        let mut remaining = to;

        let mut i = 0;
        while let Some(chunk) = self.contents.get_mut(i) {
            if chunk.len() < remaining {
                remaining -= chunk.len();
                i += 1;
            } else {
                chunk.truncate(remaining);
                self.contents.drain(i + 1..);
                return;
            }
        }

        if remaining == 1 {
            panic!("small buf");
        }

        self.contents.push(vec![0; remaining]);
    }
}
