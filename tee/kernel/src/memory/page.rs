use core::{
    alloc::Layout,
    marker::PhantomData,
    ops::{Bound, RangeBounds},
    ptr::{addr_of, addr_of_mut, copy_nonoverlapping, slice_from_raw_parts_mut, NonNull},
    sync::atomic::{AtomicU64, Ordering},
};

use alloc::alloc::{alloc, alloc_zeroed, dealloc};
use x86_64::{structures::paging::PhysFrame, PhysAddr};

use crate::{
    error::{Error, Result},
    memory::{pagetable::PageTableFlags, temporary::copy_into_page_direct},
    user::process::memory::MemoryPermissions,
};

use super::pagetable::PresentPageTableEntry;

pub struct KernelPage {
    content: NonNull<PageContent>,
    reference_count: Option<NonNull<AtomicU64>>,
}

impl KernelPage {
    pub fn uninit() -> Result<Self> {
        let content = unsafe { alloc(Layout::new::<PageContent>()) };
        let content = NonNull::new(content.cast()).ok_or_else(|| Error::no_mem(()))?;
        Ok(Self {
            content,
            reference_count: None,
        })
    }

    pub fn zeroed() -> Result<Self> {
        let content = unsafe { alloc_zeroed(Layout::new::<PageContent>()) };
        let content = NonNull::new(content.cast()).ok_or_else(|| Error::no_mem(()))?;
        Ok(Self {
            content,
            reference_count: None,
        })
    }

    /// Create a shallow copy of the page.
    pub fn clone(&mut self) -> Result<Self> {
        let reference_count = if let Some(reference_count) = self.reference_count {
            reference_count
        } else {
            // Initialize the reference count.
            let reference_count = unsafe { alloc(Layout::new::<AtomicU64>()) };
            let reference_count = reference_count.cast::<AtomicU64>();
            unsafe {
                reference_count.write(AtomicU64::new(1));
            }
            NonNull::new(reference_count).ok_or_else(|| Error::no_mem(()))?
        };

        // Increase the reference count.
        let reference_count = unsafe { reference_count.as_ref() };
        reference_count.fetch_add(1, Ordering::SeqCst);

        Ok(Self {
            content: self.content,
            reference_count: self.reference_count,
        })
    }

    /// Returns whether page's content may be modified.
    pub fn mutable(&self) -> bool {
        self.reference_count.is_none()
    }

    pub fn make_mut(&mut self) -> Result<()> {
        let Some(reference_count) = self.reference_count.take() else {
            // If there is no reference count, we don't need to do anything.
            return Ok(());
        };

        let rc = {
            let reference_count = unsafe { reference_count.as_ref() };
            reference_count.load(Ordering::SeqCst)
        };
        if rc == 1 {
            // If the reference count is one, we can take ownership of the memory.

            // Deallocate the reference count.
            unsafe {
                dealloc(reference_count.as_ptr().cast(), Layout::new::<AtomicU64>());
            }

            return Ok(());
        }

        let content = unsafe { alloc(Layout::new::<PageContent>()) };
        let content =
            NonNull::new(content.cast::<PageContent>()).ok_or_else(|| Error::no_mem(()))?;
        // Copy the memory to the new allocation.
        unsafe {
            copy_nonoverlapping(self.content.as_ptr(), content.as_ptr(), 1);
        }

        // Replace self with the new allocation.
        *self = Self {
            content,
            reference_count: None,
        };

        Ok(())
    }

    pub fn content(&self) -> VolatileBytes<ReadOnly> {
        VolatileBytes {
            access: ReadOnly(()),
            content: self.content,
            _marker: PhantomData,
        }
    }

    pub fn content_make_mut(&mut self) -> Result<VolatileBytes<ReadWrite>> {
        self.make_mut()?;

        Ok(VolatileBytes {
            access: ReadWrite(()),
            content: self.content,
            _marker: PhantomData,
        })
    }

    pub fn frame(&self) -> PhysFrame {
        let vaddr = self.content.as_ptr() as u64;
        let paddr = vaddr - 0xffff_8080_0000_0000;
        PhysFrame::from_start_address(PhysAddr::new(paddr)).unwrap()
    }
}

unsafe impl Send for KernelPage {}
unsafe impl Sync for KernelPage {}

impl Drop for KernelPage {
    fn drop(&mut self) {
        if let Some(reference_count) = self.reference_count {
            // Decrease the reference count.
            let rc = {
                let reference_count = unsafe { reference_count.as_ref() };
                reference_count.fetch_sub(1, Ordering::SeqCst)
            };

            // Don't free any memory if the reference count didn't hit 0.
            if rc != 1 {
                return;
            }

            // Free the reference count.
            unsafe {
                dealloc(reference_count.as_ptr().cast(), Layout::new::<AtomicU64>());
            }
        }

        // Free the page's memory.
        unsafe {
            dealloc(self.content.as_ptr().cast(), Layout::new::<PageContent>());
        }
    }
}

pub struct UserPage {
    perms: MemoryPermissions,
    page: KernelPage,
}

impl UserPage {
    pub fn entry(&self) -> PresentPageTableEntry {
        let mut flags = PageTableFlags::USER;
        flags.set(
            PageTableFlags::WRITABLE,
            self.perms.contains(MemoryPermissions::WRITE) && self.page.mutable(),
        );
        flags.set(
            PageTableFlags::EXECUTABLE,
            self.perms.contains(MemoryPermissions::EXECUTE),
        );
        PresentPageTableEntry::new(self.page.frame(), flags)
    }

    pub fn content(&self) -> VolatileBytes<ReadOnly> {
        self.page.content()
    }

    pub fn content_make_mut(&mut self) -> Result<VolatileBytes<ReadWrite>> {
        self.page.content_make_mut()
    }

    fn make_mut(&mut self) -> Result<()> {
        self.page.make_mut()
    }

    pub fn perms(&self) -> MemoryPermissions {
        self.perms
    }

    pub fn set_perms(&mut self, perms: MemoryPermissions) {
        self.perms = perms;
    }
}

#[repr(C, align(4096))]
struct PageContent(pub [u8; 4096]);

/// A view into some bytes that allows racy accesses.
///
/// Partly inspired by the `volatile` crate
#[derive(Clone, Copy)]
pub struct VolatileBytes<'a, A>
where
    A: Access,
{
    access: A,
    content: NonNull<PageContent>,
    _marker: PhantomData<&'a PageContent>,
}

impl<'a, A> VolatileBytes<'a, A>
where
    A: Access,
{
    pub fn index(self, range: impl RangeBounds<usize>) -> NonNull<[u8]> {
        let start = match range.start_bound() {
            Bound::Included(&idx) => idx,
            Bound::Excluded(&idx) => idx + 1,
            Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            Bound::Included(&idx) => idx + 1,
            Bound::Excluded(&idx) => idx,
            Bound::Unbounded => 0x1000,
        };
        assert!(start < 0x1000);
        assert!(end <= 0x1000);

        let len = end.saturating_sub(start);

        let base = self.content.as_ptr().cast::<u8>();
        let data = unsafe { base.add(start) };
        let offset = slice_from_raw_parts_mut(data, len);
        unsafe { NonNull::new_unchecked(offset) }
    }
}

impl<'a> VolatileBytes<'a, ReadWrite> {
    pub fn read_only(self) -> VolatileBytes<'a, ReadOnly> {
        VolatileBytes {
            access: ReadOnly(()),
            content: self.content,
            _marker: PhantomData,
        }
    }

    pub fn copy_from(self, src: VolatileBytes<'a, impl Access>) {
        let src = src.content.as_ptr();
        let dest = self.content.as_ptr();
        unsafe {
            let src = unsafe { addr_of!((*src).0) };
            let dst = unsafe { addr_of_mut!((*dest).0) };
            copy_into_page_direct(src, dst);
        }
    }
}

trait Access: Sized + Send + Sync {}

pub struct ReadOnly(());

impl Access for ReadOnly {}

pub struct ReadWrite(());

impl Access for ReadWrite {}
