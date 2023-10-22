use core::{
    alloc::{AllocError, Allocator, Layout},
    cell::Cell,
    mem::{align_of, size_of},
    ptr::{addr_of_mut, NonNull},
};

use crate::spin::mutex::Mutex;

use super::fallback_allocator::LimitedAllocator;

pub struct FixedSizeAllocator<A, const N: usize> {
    allocator: A,
    block: Mutex<Option<NonNull<Block<N>>>>,
}

impl<A, const N: usize> FixedSizeAllocator<A, N> {
    pub const fn new(allocator: A) -> Self {
        Self {
            allocator,
            block: Mutex::new(None),
        }
    }
}

unsafe impl<A, const N: usize> LimitedAllocator for FixedSizeAllocator<A, N>
where
    A: Allocator,
{
    fn can_allocate(&self, layout: Layout) -> bool {
        layout.size() <= N && layout.align() <= align_of::<Entry<N>>()
    }
}

unsafe impl<A, const N: usize> Allocator for FixedSizeAllocator<A, N>
where
    A: Allocator,
{
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if !self.can_allocate(layout) {
            return Err(AllocError);
        }

        let mut guard = self.block.lock();

        let block = if let Some(block) = &mut *guard {
            block
        } else {
            guard.insert(Block::new(&self.allocator)?)
        };

        let mut block = unsafe { block.as_mut() };
        loop {
            let res = block.allocate(layout);
            if let Ok(res) = res {
                break Ok(res);
            } else {
                block = block.make_mut(&self.allocator)?;
            }
        }
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        let guard = self.block.lock();

        let mut first_block = guard.unwrap();
        let mut block = unsafe { first_block.as_mut() };
        loop {
            if block.contains(ptr) {
                unsafe {
                    block.deallocate(ptr, layout);
                }
                return;
            }
            block = block.next().unwrap();
        }
    }
}

unsafe impl<A, const N: usize> Send for FixedSizeAllocator<A, N> {}

unsafe impl<A, const N: usize> Sync for FixedSizeAllocator<A, N> {}

struct Block<const N: usize> {
    next: Option<NonNull<Self>>,
    memory: NonNull<[u8]>,
    free_list: Cell<Option<NonNull<Entry<N>>>>,
}

impl<const N: usize> Block<N> {
    pub fn new<A>(allocator: &A) -> Result<NonNull<Self>, AllocError>
    where
        A: Allocator,
    {
        const MIN_ENTRIES: usize = 128;

        let block_layout = Layout::new::<Self>();
        let entries_layout = Layout::array::<Entry<N>>(MIN_ENTRIES).unwrap();
        let (combined, entries_offset) = block_layout.extend(entries_layout).unwrap();

        let ptr = allocator.allocate(combined)?;
        let total_len = ptr.len();
        let len_for_entries = total_len - entries_offset;
        let num_entries = len_for_entries / size_of::<Entry<N>>();

        let block_ptr = ptr.as_ptr().cast::<Self>();
        let entry_ptr = unsafe { ptr.as_ptr().cast::<Entry<N>>().byte_add(entries_offset) };

        let mut prev = None;
        for i in 0..num_entries {
            let entry_ptr = unsafe { entry_ptr.add(i) };
            let entry_ptr = unsafe { NonNull::new_unchecked(entry_ptr) };
            let entry = Entry { next: prev };
            unsafe {
                entry_ptr.as_ptr().write(entry);
            }
            prev = Some(entry_ptr);
        }

        #[cfg(sanitize = "address")]
        unsafe {
            crate::sanitize::mark(entry_ptr.cast(), len_for_entries, false);
        }

        let block = Block {
            next: None,
            memory: ptr,
            free_list: Cell::new(prev),
        };
        unsafe {
            block_ptr.write(block);
        }

        Ok(unsafe { NonNull::new_unchecked(block_ptr) })
    }

    pub fn next(&mut self) -> Option<&mut Self> {
        let mut ptr = self.next?;
        Some(unsafe { ptr.as_mut() })
    }

    pub fn make_mut<A>(&mut self, allocator: &A) -> Result<&mut Self, AllocError>
    where
        A: Allocator,
    {
        let ptr = self.next;
        if let Some(mut ptr) = ptr {
            return Ok(unsafe { ptr.as_mut() });
        }

        let mut ptr = Self::new(allocator)?;
        self.next = Some(ptr);
        Ok(unsafe { ptr.as_mut() })
    }

    pub fn contains(&self, ptr: NonNull<u8>) -> bool {
        let start_ptr = self.memory.as_ptr().cast::<u8>();
        let end_ptr = unsafe { start_ptr.add(self.memory.len()) };
        (start_ptr..end_ptr).contains(&ptr.as_ptr())
    }
}

unsafe impl<const N: usize> Allocator for Block<N> {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        assert!(layout.size() <= N);
        assert!(layout.align() <= align_of::<Entry<N>>());

        assert_eq!(layout.size() % layout.align(), 0);

        let ptr = self.free_list.get().ok_or(AllocError)?;
        let next_ptr = unsafe { (*ptr.as_ptr()).next() };
        self.free_list.set(next_ptr);

        #[cfg(sanitize = "address")]
        unsafe {
            crate::sanitize::mark(
                ptr.as_ptr().cast_const().cast::<_>(),
                layout.size().next_multiple_of(8),
                true,
            );
        }

        let ptr = core::ptr::slice_from_raw_parts_mut(ptr.as_ptr().cast::<u8>(), layout.size());
        Ok(NonNull::new(ptr).unwrap())
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, _layout: Layout) {
        #[cfg(sanitize = "address")]
        unsafe {
            crate::sanitize::mark(ptr.as_ptr().cast_const().cast::<_>(), N, false);
        }

        let entry = ptr.cast::<Entry<N>>();

        let current_free_list = self.free_list.get();

        unsafe {
            (*entry.as_ptr()).set_next(current_free_list);
        }

        self.free_list.set(Some(entry));
    }
}

#[repr(C, align(8))]
union Entry<const N: usize> {
    next: Option<NonNull<Self>>,
    raw: [u8; N],
}

impl<const N: usize> Entry<N> {
    #[cfg(not(sanitize = "address"))]
    unsafe fn next(&self) -> Option<NonNull<Self>> {
        unsafe { self.next }
    }

    #[cfg(sanitize = "address")]
    unsafe fn next(&self) -> Option<NonNull<Self>> {
        use core::arch::asm;

        let next: *mut Entry<N>;
        unsafe {
            asm!("mov {next}, qword ptr [{this}]", this = in(reg) self, next = lateout(reg) next);
        }
        NonNull::new(next)
    }

    #[cfg(not(sanitize = "address"))]
    fn set_next(&mut self, next: Option<NonNull<Self>>) {
        self.next = next;
    }

    #[cfg(sanitize = "address")]
    fn set_next(&mut self, next: Option<NonNull<Self>>) {
        use core::{arch::asm, ptr::null_mut};

        let next = next.map_or(null_mut(), NonNull::as_ptr);
        unsafe {
            asm!("mov qword ptr [{this}], {next}", this = in(reg) self, next = in(reg) next);
        }
    }
}
