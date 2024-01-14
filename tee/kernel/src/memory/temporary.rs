use core::{
    arch::{
        asm,
        x86_64::{
            _mm256_load_si256, _mm256_setzero_si256, _mm256_stream_si256, _mm256_zeroall,
            _mm_sfence, _rdtsc,
        },
    },
    cell::{RefCell, RefMut},
    panic::Location,
};

use crate::spin::{lazy::Lazy, mutex::Mutex};
use constants::virtual_address::TEMPORARY;
use log::{debug, info};
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

#[inline(always)]
fn get_fast_mapping(frame: PhysFrame) -> Option<*mut [u8; 4096]> {
    (0x0000_0200_0000_0000..=0x0000_02ff_ffff_ffff)
        .contains(&frame.start_address().as_u64())
        .then(|| {
            (frame.start_address().as_u64() - 0x0000_0200_0000_0000 + 0xffff_8080_0000_0000)
                as *mut _
        })
}

/// Fill a frame with zeros.
///
/// # Safety
///
/// Writing to the frame must be safe.
#[inline(always)]
pub unsafe fn zero_frame(frame: PhysFrame) -> Result<()> {
    if let Some(dst) = get_fast_mapping(frame) {
        unsafe { zero_frame_direct(dst) };
        Ok(())
    } else {
        unsafe { zero_frame_slow(frame) }
    }
}

#[inline(never)]
#[cold]
unsafe fn zero_frame_slow(frame: PhysFrame) -> Result<()> {
    let mut mapping = TemporaryMapping::new(frame)?;
    unsafe {
        zero_frame_direct(mapping.as_mut_ptr());
    }
    Ok(())
}

#[inline(always)]
unsafe fn zero_frame_direct(dst: *mut [u8; 4096]) {
    unsafe {
        asm! {
            "vpxor ymm0, ymm0, ymm0",
            "66:",
            "vmovntdq [{dst}], ymm0",
            "vmovntdq [{dst}+32], ymm0",
            "add {dst}, 64",
            "loop 66b",
            "sfence",
            dst = inout(reg) dst => _,
            inout("ecx") 4096 / 64 => _,
            options(nostack),
        }
    }
}

/// Copy bytes into a frame.
///
/// # Safety
///
/// Writing to the frame must be safe.
#[inline(always)]
pub unsafe fn copy_into_frame(frame: PhysFrame, bytes: &[u8; 0x1000]) -> Result<()> {
    if let Some(dst) = get_fast_mapping(frame) {
        unsafe { copy_into_page_direct(bytes, dst) };
        Ok(())
    } else {
        unsafe { copy_into_frame_slow(frame, bytes) }
    }
}

#[inline(never)]
#[cold]
pub unsafe fn copy_into_frame_slow(frame: PhysFrame, bytes: &[u8; 0x1000]) -> Result<()> {
    let mut mapping = TemporaryMapping::new(frame)?;
    unsafe {
        copy_into_page_direct(bytes.as_ptr().cast(), mapping.as_mut_ptr());
    }
    Ok(())
}

#[inline(always)]
unsafe fn copy_into_page_direct(src: *const [u8; 4096], dst: *mut [u8; 4096]) {
    assert!(dst.is_aligned_to(32));

    if src.is_aligned_to(32) {
        unsafe {
            asm! {
                "66:",
                "vmovdqa ymm0, [{src}]",
                "vmovdqa [{dst}], ymm0",
                "add {src}, 32",
                "add {dst}, 32",
                "loop 66b",
                src = inout(reg) src => _,
                dst = inout(reg) dst => _,
                inout("ecx") 4096 / 32 => _,
                options(nostack),
            }
        }
    } else {
        unsafe {
            asm! {
                "66:",
                "vmovdqu ymm0, [{src}]",
                "vmovdqa [{dst}], ymm0",
                "add {src}, 32",
                "add {dst}, 32",
                "loop 66b",
                src = inout(reg) src => _,
                dst = inout(reg) dst => _,
                inout("ecx") 4096 / 32 => _,
                options(nostack),
            }
        }
    }
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
            PresentPageTableEntry::new(frame, PageTableFlags::WRITABLE | PageTableFlags::LOCAL);
        // let prev = PerCpu::get()
        // .temporary_mapping_used
        // .borrow_mut()
        // .replace(Location::caller());
        // if let Some(prev) = prev {
        // panic!("{prev}");
        // }

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

        // *PerCpu::get().temporary_mapping_used.borrow_mut() = None;
        // debug!("unmap {:?}", self.page);
        unsafe {
            unmap_page(*self.page);
        }

        flush_current_pcid();
    }
}

fn flush_current_pcid() {
    return;

    static INVLPGB: Lazy<Invlpgb> = Lazy::new(|| Invlpgb::new().expect("invlpgb not supported"));

    let (_, pcid) = Cr3::read_pcid();
    unsafe {
        INVLPGB.build().pcid(pcid).flush();
    }

    INVLPGB.tlbsync();
}
