use core::{
    cell::{RefCell, RefMut},
    panic::Location,
};

use crate::spin::{lazy::Lazy, mutex::Mutex};
use constants::virtual_address::TEMPORARY;
use log::debug;
use x86_64::{
    instructions::tlb::Invlpgb,
    registers::control::Cr3,
    structures::paging::{page::PageRangeInclusive, Page, PhysFrame, Size4KiB},
};

use crate::{
    error::Result,
    memory::{
        frame::FRAME_ALLOCATOR,
        pagetable::{map_page, PresentPageTableEntry},
    },
    per_cpu::PerCpu,
};

use super::pagetable::{unmap_page, PageTableFlags};

/// Fill a frame with zeros.
///
/// # Safety
///
/// Writing to the frame must be safe.
pub unsafe fn zero_frame(frame: PhysFrame) -> Result<()> {
    let mut mapping = TemporaryMapping::new(frame)?;
    unsafe {
        core::intrinsics::volatile_set_memory(mapping.as_mut_ptr(), 0, 1);
    }
    Ok(())
}

/// Copy bytes into a frame.
///
/// # Safety
///
/// Writing to the frame must be safe.
pub unsafe fn copy_into_frame(frame: PhysFrame, bytes: &[u8; 0x1000]) -> Result<()> {
    let mut mapping = TemporaryMapping::new(frame)?;
    unsafe {
        core::intrinsics::volatile_copy_nonoverlapping_memory(mapping.as_mut_ptr(), bytes, 1);
    }
    Ok(())
}

struct TemporaryMapping {
    page: RefMut<'static, Page>,
}

impl TemporaryMapping {
    #[track_caller]
    pub fn new(frame: PhysFrame) -> Result<Self> {
        static PAGES: Lazy<Mutex<PageRangeInclusive<Size4KiB>>> =
            Lazy::new(|| Mutex::new(TEMPORARY.into_iter()));

        let per_cpu = PerCpu::get().temporary_mapping.get_or_init(|| {
            let page = PAGES.lock().next().unwrap();
            debug!("{page:?}");
            RefCell::new(page)
        });

        let page = per_cpu.borrow_mut();
        let entry =
            PresentPageTableEntry::new(frame, PageTableFlags::WRITABLE | PageTableFlags::GLOBAL);
        let prev = PerCpu::get()
            .temporary_mapping_used
            .borrow_mut()
            .replace(Location::caller());
        if let Some(prev) = prev {
            panic!("{prev}");
        }

        flush_current_pcid();

        // debug!("map {page:?}");
        unsafe {
            map_page(*page, entry, &mut (&FRAME_ALLOCATOR))?;
        }

        flush_current_pcid();

        Ok(Self { page })
    }

    pub fn as_mut_ptr(&mut self) -> *mut [u8; 4096] {
        self.page.start_address().as_mut_ptr()
    }
}

impl Drop for TemporaryMapping {
    fn drop(&mut self) {
        flush_current_pcid();

        *PerCpu::get().temporary_mapping_used.borrow_mut() = None;
        // debug!("unmap {:?}", self.page);
        unsafe {
            unmap_page(*self.page);
        }

        flush_current_pcid();
    }
}

fn flush_current_pcid() {
    static INVLPGB: Lazy<Invlpgb> = Lazy::new(|| Invlpgb::new().expect("invlpgb not supported"));

    let (_, pcid) = Cr3::read_pcid();
    unsafe {
        INVLPGB.build().pcid(pcid).flush();
    }

    INVLPGB.tlbsync();
}
