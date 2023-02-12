use alloc::sync::Arc;
use constants::{physical_address, virtual_address};
use spin::Lazy;
use x86_64::structures::paging::{frame::PhysFrameRangeInclusive, page::PageRangeInclusive};

use crate::{
    error::Result,
    fs::node::StaticFile,
    memory::{
        frame::DUMB_FRAME_ALLOCATOR,
        pagetable::{map_page, PageTableFlags},
    },
};

use self::node::{Directory, ROOT_NODE};

pub mod node;
pub mod path;

pub use path::{FileName, Path, PathSegment};

pub fn init() -> Result<()> {
    load_init()?;
    load_input()
}

fn load_init() -> Result<()> {
    let dev = ROOT_NODE.mkdir(FileName::new(b"bin").unwrap(), false)?;
    dev.create(
        FileName::new(b"init").unwrap(),
        Arc::new(StaticFile::new(&INIT, true)),
        true,
    )?;
    Ok(())
}

fn load_input() -> Result<()> {
    let dev = ROOT_NODE.mkdir(FileName::new(b"dev").unwrap(), false)?;
    dev.create(
        FileName::new(b"input").unwrap(),
        Arc::new(StaticFile::new(&INPUT, false)),
        true,
    )?;
    Ok(())
}

static INIT: Lazy<&'static [u8]> = Lazy::new(|| {
    let pages = virtual_address::INIT.into_iter();
    let frames = physical_address::INIT.into_iter();
    load_static_file(pages, frames)
});

static INPUT: Lazy<&'static [u8]> = Lazy::new(|| {
    let pages = virtual_address::INPUT.into_iter();
    let frames = physical_address::INPUT.into_iter();
    load_static_file(pages, frames)
});

fn load_static_file(
    mut pages: PageRangeInclusive,
    mut frames: PhysFrameRangeInclusive,
) -> &'static [u8] {
    let header_page = pages.next().unwrap();
    let header_frame = frames.next().unwrap();

    unsafe {
        map_page(
            header_page,
            header_frame,
            PageTableFlags::GLOBAL,
            &mut &DUMB_FRAME_ALLOCATOR,
        );
    }

    let len = unsafe {
        header_page
            .start_address()
            .as_ptr::<usize>()
            .read_volatile()
    };

    let num_pages = len.div_ceil(0x1000);
    for _ in 0..num_pages {
        let input_page = pages.next().unwrap();
        let input_frame = frames.next().unwrap();

        unsafe {
            map_page(
                input_page,
                input_frame,
                PageTableFlags::GLOBAL,
                &mut &DUMB_FRAME_ALLOCATOR,
            );
        }
    }

    let first_input_page = header_page + 1;
    let ptr = first_input_page.start_address().as_ptr();
    unsafe { core::slice::from_raw_parts(ptr, len) }
}
