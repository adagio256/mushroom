use core::{
    alloc::{AllocError, Allocator, Layout},
    cell::Cell,
    mem::{align_of, size_of},
    ptr::NonNull,
};

use alloc::vec::Vec;
use x86_64::align_down;

use crate::spin::mutex::Mutex;

use super::fallback_allocator::LimitedAllocator;

struct FixedSizeAllocatorState<A, const N: usize>
where
    A: Allocator,
{
    blocks: Vec<NonNull<Block<N>>, A>,
    /// The index of the last block where memory was allocated from.
    last_block_idx: usize,
}

impl<A, const N: usize> FixedSizeAllocatorState<A, N>
where
    A: Allocator,
{
    pub const fn new(allocator: A) -> Self {
        Self {
            blocks: Vec::new_in(allocator),
            last_block_idx: 0,
        }
    }
}

pub struct FixedSizeAllocator<A, const N: usize>
where
    A: Allocator,
{
    blocks: Mutex<FixedSizeAllocatorState<A, N>>,
}

impl<A, const N: usize> FixedSizeAllocator<A, N>
where
    A: Allocator + Copy,
{
    pub const fn new(allocator: A) -> Self {
        Self {
            blocks: Mutex::new(FixedSizeAllocatorState::new(allocator)),
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

        let mut guard = self.blocks.lock();

        if let Some((idx, ptr)) = guard
            .blocks
            .iter()
            .skip(guard.last_block_idx)
            .chain(guard.blocks.iter().take(guard.last_block_idx))
            .map(|p| unsafe { p.as_ref() })
            .enumerate()
            .find_map(|(idx, p)| {
                let ptr = p.allocate(layout).ok()?;
                Some((idx, ptr))
            })
        {
            guard.last_block_idx = (guard.last_block_idx + idx) % guard.blocks.len();
            Ok(ptr)
        } else {
            let block = Block::new(guard.blocks.allocator())?;
            let b = unsafe { block.as_ref() };
            let ptr = b.allocate(layout).unwrap();
            guard.last_block_idx = guard.blocks.len();
            guard.blocks.push(block);

            Ok(ptr)
        }
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        let mut guard = self.blocks.lock();

        let block = align_down(ptr.as_ptr() as u64, BLOCK_SIZE as u64);
        let block_ptr = block as *mut Block<N>;
        let block = unsafe { &mut *(block_ptr) };

        assert!(block.contains(ptr));
        unsafe {
            block.deallocate(ptr, layout);
        }

        if block.used.get() == 0 {
            unsafe {
                assert!(block.try_free(guard.blocks.allocator()));
            }

            guard.blocks.retain(|b| b.as_ptr() != block_ptr);
        }
    }
}

unsafe impl<A, const N: usize> Send for FixedSizeAllocator<A, N> where A: Allocator {}

unsafe impl<A, const N: usize> Sync for FixedSizeAllocator<A, N> where A: Allocator {}

const BLOCK_SIZE: usize = 1024 * 1024 * 2;

struct Block<const N: usize> {
    memory: NonNull<[u8]>,
    free_list: Cell<Option<NonNull<Entry<N>>>>,
    used: Cell<usize>,
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
        let combined = combined.align_to(BLOCK_SIZE).unwrap().pad_to_align();

        let ptr = allocator.allocate(combined)?;
        assert!(ptr.len() <= BLOCK_SIZE);
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
            memory: ptr,
            free_list: Cell::new(prev),
            used: Cell::new(0),
        };
        unsafe {
            block_ptr.write(block);
        }

        Ok(unsafe { NonNull::new_unchecked(block_ptr) })
    }

    pub fn contains(&self, ptr: NonNull<u8>) -> bool {
        let start_ptr = self.memory.as_ptr().cast::<u8>();
        let end_ptr = unsafe { start_ptr.add(self.memory.len()) };
        (start_ptr..end_ptr).contains(&ptr.as_ptr())
    }

    pub unsafe fn try_free(&mut self, allocator: &impl Allocator) -> bool {
        if self.used.get() > 0 {
            return false;
        }

        unsafe {
            allocator.deallocate(
                self.memory.cast(),
                Layout::array::<u8>(self.memory.len())
                    .unwrap()
                    .align_to(BLOCK_SIZE)
                    .unwrap(),
            );
        }

        true
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

        self.used.set(self.used.get() + 1);

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

        self.used.set(self.used.get() - 1);

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
