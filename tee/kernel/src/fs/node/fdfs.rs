use alloc::{
    format,
    sync::{Arc, Weak},
    vec::Vec,
};

use super::{new_ino, DirEntry, Directory, File, FileAccessContext, Node};
use crate::{
    error::{Error, Result},
    fs::{
        node::DirEntryName,
        path::{FileName, Path},
    },
    spin::mutex::Mutex,
    user::process::{
        fd::FileDescriptor,
        syscall::args::{FdNum, FileMode, FileType, FileTypeAndMode, Stat, Timespec},
    },
};

pub fn new(parent: Weak<dyn Directory>, mode: FileMode) -> Arc<dyn Directory> {
    Arc::new(FdFsRoot {
        ino: new_ino(),
        parent,
        mode: Mutex::new(mode),
    })
}

struct FdFsRoot {
    ino: u64,
    parent: Weak<dyn Directory>,
    mode: Mutex<FileMode>,
}

impl Directory for FdFsRoot {
    fn parent(&self) -> Result<Arc<dyn Directory>> {
        self.parent
            .clone()
            .upgrade()
            .ok_or_else(|| Error::no_ent(()))
    }

    fn stat(&self) -> Stat {
        let mode = *self.mode.lock();
        let mode = FileTypeAndMode::new(FileType::Dir, mode);

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
            atime: Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            mtime: Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            ctime: Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
        }
    }

    fn set_mode(&self, mode: FileMode) {
        *self.mode.lock() = mode;
    }

    fn get_node(&self, file_name: &FileName, ctx: &FileAccessContext) -> Result<Node> {
        let file_name = file_name.as_bytes();
        let file_name = core::str::from_utf8(file_name).map_err(|_| Error::no_ent(()))?;
        let fd_num = file_name.parse().map_err(|_| Error::no_ent(()))?;
        let fd_num = FdNum::new(fd_num);
        let fd = ctx.fdtable.get(fd_num)?;
        Ok(Node::Fd(FdNode(fd)))
    }

    fn create_file(
        &self,
        _file_name: FileName<'static>,
        _mode: FileMode,
        _create_new: bool,
    ) -> Result<Arc<dyn File>> {
        Err(Error::no_ent(()))
    }

    fn create_dir(
        &self,
        _file_name: FileName<'static>,
        _mode: FileMode,
    ) -> Result<Arc<dyn Directory>> {
        Err(Error::no_ent(()))
    }

    fn create_link(
        &self,
        _file_name: FileName<'static>,
        _target: Path,
        _create_new: bool,
    ) -> Result<()> {
        Err(Error::no_ent(()))
    }

    fn hard_link(&self, _file_name: FileName<'static>, _node: Node) -> Result<()> {
        Err(Error::no_ent(()))
    }

    fn mount(&self, _file_name: FileName<'static>, _node: Node) -> Result<()> {
        Err(Error::no_ent(()))
    }

    fn list_entries(&self, ctx: &FileAccessContext) -> Vec<DirEntry> {
        ctx.fdtable
            .nums()
            .into_iter()
            .map(|(num, fd)| DirEntry {
                ino: fd.ino(),
                ty: FileType::File,
                name: DirEntryName::FileName(
                    FileName::new(format!("{num}").as_bytes())
                        .unwrap()
                        .into_owned(),
                ),
            })
            .collect()
    }

    fn delete_non_dir(&self, _file_name: FileName<'static>) -> Result<()> {
        Err(Error::no_ent(()))
    }

    fn delete_dir(&self, _file_name: FileName<'static>) -> Result<()> {
        Err(Error::no_ent(()))
    }
}

#[derive(Clone)]
pub struct FdNode(FileDescriptor);

impl FdNode {
    pub fn stat(&self) -> Stat {
        todo!()
    }
}

impl From<FdNode> for FileDescriptor {
    fn from(value: FdNode) -> Self {
        value.0
    }
}
