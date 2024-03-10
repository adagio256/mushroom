use core::{
    cmp::Ordering,
    fmt::{self, Debug},
    iter::Step,
    ops::Index,
    sync::atomic::AtomicUsize,
};

use alloc::{vec, vec::Vec};
use usize_conversions::{usize_from, FromUsize};
use x86_64::{structures::paging::Page, VirtAddr};

pub struct PageMap<T> {
    chunks: Vec<Chunk<T>>,
    last_index: AtomicUsize,
}

impl<T> PageMap<T> {
    pub const fn new() -> Self {
        Self {
            chunks: Vec::new(),
            last_index: AtomicUsize::new(0),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    #[inline]
    fn find_chunk_idx_impl(&self, page: Page) -> Result<usize, usize> {
        if self.chunks.len() < 16 {
            for (i, chunk) in self.chunks.iter().enumerate() {
                match chunk.start.cmp(&page) {
                    Ordering::Less => {}
                    Ordering::Equal => return Ok(i),
                    Ordering::Greater => return Err(i),
                }
            }
            return Err(self.chunks.len());
        }

        self.chunks.binary_search_by_key(&page, Chunk::start)
    }

    #[inline]
    fn find_chunk_idx(&self, page: Page) -> Result<usize, usize> {
        let idx = self.last_index.load(core::sync::atomic::Ordering::Relaxed);
        if let Some(chunk) = self.chunks.get(idx) {
            match chunk.start.cmp(&page) {
                Ordering::Less => {
                    let next_idx = idx + 1;
                    if let Some(next_chunk) = self.chunks.get(next_idx) {
                        match next_chunk.start.cmp(&page) {
                            Ordering::Less => {}
                            Ordering::Equal => return Ok(next_idx),
                            Ordering::Greater => return Err(next_idx),
                        }
                    }
                }
                Ordering::Equal => return Ok(idx),
                Ordering::Greater => {}
            }
        }

        let res = self.find_chunk_idx_impl(page);
        let idx = match res {
            Ok(idx) => idx,
            Err(idx) => idx,
        };
        self.last_index
            .store(idx, core::sync::atomic::Ordering::Relaxed);
        res
    }

    pub fn get(&self, page: Page) -> Option<&T> {
        let res = self.find_chunk_idx(page);
        match res {
            Ok(idx) => Some(&self.chunks[idx].values[0]),
            Err(idx) => {
                let idx = idx.checked_sub(1)?;
                let chunk = &self.chunks[idx];
                let value_idx = usize_from(page - chunk.start);
                chunk.values.get(value_idx)
            }
        }
    }

    pub fn get_mut(&mut self, page: Page) -> Option<&mut T> {
        let res = self.find_chunk_idx(page);
        match res {
            Ok(idx) => Some(&mut self.chunks[idx].values[0]),
            Err(idx) => {
                let idx = idx.checked_sub(1)?;
                let chunk = &mut self.chunks[idx];
                let value_idx = usize_from(page - chunk.start);
                chunk.values.get_mut(value_idx)
            }
        }
    }

    pub fn chunks_mut(&mut self) -> impl Iterator<Item = (Page, impl Iterator<Item = &mut T>)> {
        self.chunks
            .iter_mut()
            .map(|c| (c.start, c.values.iter_mut()))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (Page, &mut T)> {
        self.chunks
            .iter_mut()
            .flat_map(|c| (c.start..).zip(c.values.iter_mut()))
    }

    pub fn insert(&mut self, page: Page, value: T) {
        let mut cursor = self.insert_many(page);
        cursor.insert(value);
    }

    pub fn insert_iter(&mut self, page: Page, values: impl IntoIterator<Item = T>) {
        let mut cursor = self.insert_many(page);
        for value in values {
            cursor.insert(value);
        }
    }

    pub fn insert_many(&mut self, mut page: Page) -> InsertCursor<'_, T> {
        let res = self.find_chunk_idx(page);
        let mut idx = match res {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        };
        InsertCursor {
            page_map: self,
            idx,
            page,
        }
    }

    pub fn remove(&mut self, page: Page) {
        self.remove_many(page, 1)
    }

    pub fn remove_many(&mut self, page: Page, len: usize) {
        // FIXME: This doesn't remove the last possible page. Do we care about this?
        let end = Step::forward_checked(page, len)
            .unwrap_or_else(|| Page::containing_address(VirtAddr::new(!0)));

        let res = self.find_chunk_idx(page);
        let mut idx = match res {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        };

        loop {
            let Some(chunk) = self.chunks.get_mut(idx) else {
                break;
            };

            match (chunk.start.cmp(&page), chunk.end().cmp(&end)) {
                (Ordering::Less, Ordering::Equal | Ordering::Less) => {
                    chunk.values.truncate(usize_from(page - chunk.start));
                    idx += 1;
                }
                (Ordering::Less, Ordering::Greater) => {
                    let new_chunk_values = chunk.values.split_off(usize_from(end - chunk.start));
                    chunk.values.truncate(usize_from(page - chunk.start));
                    self.chunks.insert(
                        idx + 1,
                        Chunk {
                            start: end,
                            values: new_chunk_values,
                        },
                    );
                    idx += 2;
                }
                (Ordering::Equal | Ordering::Greater, Ordering::Equal | Ordering::Less) => {
                    self.chunks.remove(idx);
                }
                (Ordering::Equal, Ordering::Greater) => {
                    chunk.values.drain(..usize_from(end - chunk.start));
                    chunk.start = end;
                    idx += 1;
                }
                (_, Ordering::Greater) => break,
            }
        }
    }
}

impl<T> Index<Page> for PageMap<T> {
    type Output = T;

    fn index(&self, index: Page) -> &Self::Output {
        self.get(index).unwrap()
    }
}

impl<T> Debug for PageMap<T>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut map = f.debug_map();
        for chunk in self.chunks.iter() {
            map.entry(&chunk.start, &chunk.values);
        }
        map.finish()
    }
}

struct Chunk<V> {
    start: Page,
    values: Vec<V>,
}

impl<V> Chunk<V> {
    fn start(&self) -> Page {
        self.start
    }

    fn end(&self) -> Page {
        self.start + u64::from_usize(self.values.len())
    }
}

pub struct InsertCursor<'a, T> {
    page_map: &'a mut PageMap<T>,
    idx: usize,
    page: Page,
}

#[inline(never)]
#[cold]
fn nop() {
    unsafe {
        core::arch::asm!("nop");
    }
}

impl<T> InsertCursor<'_, T> {
    pub fn insert(&mut self, value: T) {
        nop();

        let next_page = Step::forward(self.page, 1);

        if let Some(chunk) = self.page_map.chunks.get_mut(self.idx) {
            match (chunk.start.cmp(&self.page), chunk.end().cmp(&self.page)) {
                (_, Ordering::Equal) => {
                    chunk.values.push(value);

                    if let Some(next) = self.page_map.chunks.get(self.idx + 1) {
                        if next.start == next_page {
                            let mut next = self.page_map.chunks.remove(self.idx + 1);
                            self.page_map.chunks[self.idx]
                                .values
                                .append(&mut next.values);
                        }
                    }
                }
                (_, Ordering::Less) | (Ordering::Greater, _) => {
                    if let Some(next) = self
                        .page_map
                        .chunks
                        .get_mut(self.idx)
                        .filter(|next| next.start == next_page)
                    {
                        next.start = self.page;
                        next.values.insert(0, value);
                    } else {
                        self.idx += 1;
                        self.page_map.chunks.insert(
                            self.idx,
                            Chunk {
                                start: self.page,
                                values: vec![value],
                            },
                        );
                    }
                }
                (Ordering::Less | Ordering::Equal, Ordering::Greater) => {
                    let idx = usize_from(self.page - chunk.start);
                    chunk.values[idx] = value;
                }
            }
        } else {
            self.page_map.chunks.insert(
                self.idx,
                Chunk {
                    start: self.page,
                    values: vec![value],
                },
            );
        }

        self.page = next_page;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use x86_64::VirtAddr;

    macro_rules! page {
        ($addr:expr) => {
            Page::containing_address(VirtAddr::new($addr))
        };
    }

    #[test]
    fn insert_many() {
        let mut map = PageMap::new();
        map.insert_iter(page!(0x5000), [5, 6, 7, 8]);
        assert_eq!(map.get(page!(0x4000)), None);
        assert_eq!(map[page!(0x5000)], 5);
        assert_eq!(map[page!(0x6000)], 6);
        assert_eq!(map[page!(0x7000)], 7);
        assert_eq!(map[page!(0x8000)], 8);
        assert_eq!(map.get(page!(0x9000)), None);

        // Overlapping insertion (inner).
        map.insert_iter(page!(0x6000), [16, 17]);
        assert_eq!(map.get(page!(0x4000)), None);
        assert_eq!(map[page!(0x5000)], 5);
        assert_eq!(map[page!(0x6000)], 16);
        assert_eq!(map[page!(0x7000)], 17);
        assert_eq!(map[page!(0x8000)], 8);
        assert_eq!(map.get(page!(0x9000)), None);

        // Overlapping insertion (start).
        map.insert_iter(page!(0x5000), [25, 26]);
        assert_eq!(map.get(page!(0x4000)), None);
        assert_eq!(map[page!(0x5000)], 25);
        assert_eq!(map[page!(0x6000)], 26);
        assert_eq!(map[page!(0x7000)], 17);
        assert_eq!(map[page!(0x8000)], 8);
        assert_eq!(map.get(page!(0x9000)), None);

        // Overlapping insertion (before).
        map.insert_iter(page!(0x4000), [34, 35]);
        assert_eq!(map.get(page!(0x3000)), None);
        assert_eq!(map[page!(0x4000)], 34);
        assert_eq!(map[page!(0x5000)], 35);
        assert_eq!(map[page!(0x6000)], 26);
        assert_eq!(map[page!(0x7000)], 17);
        assert_eq!(map[page!(0x8000)], 8);
        assert_eq!(map.get(page!(0x9000)), None);

        // insertion before.
        map.insert_iter(page!(0x3000), [43]);
        assert_eq!(map.get(page!(0x2000)), None);
        assert_eq!(map[page!(0x3000)], 43);
        assert_eq!(map[page!(0x4000)], 34);
        assert_eq!(map[page!(0x5000)], 35);
        assert_eq!(map[page!(0x6000)], 26);
        assert_eq!(map[page!(0x7000)], 17);
        assert_eq!(map[page!(0x8000)], 8);
        assert_eq!(map.get(page!(0x9000)), None);

        // Overlapping insertion (end).
        map.insert_iter(page!(0x7000), [57, 58]);
        assert_eq!(map.get(page!(0x2000)), None);
        assert_eq!(map[page!(0x3000)], 43);
        assert_eq!(map[page!(0x4000)], 34);
        assert_eq!(map[page!(0x5000)], 35);
        assert_eq!(map[page!(0x6000)], 26);
        assert_eq!(map[page!(0x7000)], 57);
        assert_eq!(map[page!(0x8000)], 58);
        assert_eq!(map.get(page!(0x9000)), None);

        // Overlapping insertion (after).
        map.insert_iter(page!(0x8000), [68, 69]);
        assert_eq!(map.get(page!(0x2000)), None);
        assert_eq!(map[page!(0x3000)], 43);
        assert_eq!(map[page!(0x4000)], 34);
        assert_eq!(map[page!(0x5000)], 35);
        assert_eq!(map[page!(0x6000)], 26);
        assert_eq!(map[page!(0x7000)], 57);
        assert_eq!(map[page!(0x8000)], 68);
        assert_eq!(map[page!(0x9000)], 69);
        assert_eq!(map.get(page!(0xa000)), None);

        // insertion after.
        map.insert_iter(page!(0xa000), [70]);
        assert_eq!(map.get(page!(0x2000)), None);
        assert_eq!(map[page!(0x3000)], 43);
        assert_eq!(map[page!(0x4000)], 34);
        assert_eq!(map[page!(0x5000)], 35);
        assert_eq!(map[page!(0x6000)], 26);
        assert_eq!(map[page!(0x7000)], 57);
        assert_eq!(map[page!(0x8000)], 68);
        assert_eq!(map[page!(0x9000)], 69);
        assert_eq!(map[page!(0xa000)], 70);
        assert_eq!(map.get(page!(0xb000)), None);

        // Overlapping insertion (outer).
        map.insert_iter(page!(0x2000), [82, 83, 84, 85, 86, 87, 88, 89, 80, 81]);
        assert_eq!(map.get(page!(0x1000)), None);
        assert_eq!(map[page!(0x2000)], 82);
        assert_eq!(map[page!(0x3000)], 83);
        assert_eq!(map[page!(0x4000)], 84);
        assert_eq!(map[page!(0x5000)], 85);
        assert_eq!(map[page!(0x6000)], 86);
        assert_eq!(map[page!(0x7000)], 87);
        assert_eq!(map[page!(0x8000)], 88);
        assert_eq!(map[page!(0x9000)], 89);
        assert_eq!(map[page!(0xa000)], 80);
        assert_eq!(map[page!(0xb000)], 81);
        assert_eq!(map.get(page!(0xc000)), None);

        // Overlapping insertion (hole).
        map.insert_iter(page!(0xd000), [0]);
        assert_eq!(map[page!(0xd000)], 0);
        map.insert_iter(page!(0xb000), [101, 102, 103]);
        assert_eq!(map[page!(0xb000)], 101);
        assert_eq!(map[page!(0xc000)], 102);
        assert_eq!(map[page!(0xd000)], 103);
    }

    #[test]
    fn chunk_count() {
        let mut map = PageMap::new();
        map.insert(page!(0x0000), 0);
        map.insert(page!(0x1000), 1);
        map.insert(page!(0x2000), 2);
        map.insert(page!(0x3000), 3);
        assert_eq!(map.chunks.len(), 1);
    }

    #[test]
    fn remove_many() {
        // Insert some values that the test case doesn't modify.
        macro_rules! insert_unrelated {
            ($map:expr) => {
                $map.insert_iter(page!(0x00000), [0, 1]);
                $map.insert_iter(page!(0xfe000), [0xfe, 0xff]);
                check_unrelated!($map);
            };
        }
        // Check that the values inserted in `insert_unrelated!` are still
        // correct.
        macro_rules! check_unrelated {
            ($map:expr) => {
                assert_eq!($map[page!(0x00000)], 0);
                assert_eq!($map[page!(0x01000)], 1);
                assert_eq!($map[page!(0xfe000)], 0xfe);
                assert_eq!($map[page!(0xff000)], 0xff);
            };
        }

        // remove all
        let mut map = PageMap::new();
        insert_unrelated!(map);
        map.insert_iter(page!(0x5000), [5, 6, 7, 8]);
        map.remove_many(page!(0x5000), 4);
        assert_eq!(map.get(page!(0x5000)), None);
        assert_eq!(map.get(page!(0x6000)), None);
        assert_eq!(map.get(page!(0x7000)), None);
        assert_eq!(map.get(page!(0x8000)), None);
        check_unrelated!(map);

        // remove all, start before
        let mut map = PageMap::new();
        insert_unrelated!(map);
        map.insert_iter(page!(0x5000), [5, 6, 7, 8]);
        map.remove_many(page!(0x4000), 5);
        assert_eq!(map.get(page!(0x5000)), None);
        assert_eq!(map.get(page!(0x6000)), None);
        assert_eq!(map.get(page!(0x7000)), None);
        assert_eq!(map.get(page!(0x8000)), None);
        check_unrelated!(map);

        // remove all, end after
        let mut map = PageMap::new();
        insert_unrelated!(map);
        map.insert_iter(page!(0x5000), [5, 6, 7, 8]);
        map.remove_many(page!(0x5000), 5);
        assert_eq!(map.get(page!(0x5000)), None);
        assert_eq!(map.get(page!(0x6000)), None);
        assert_eq!(map.get(page!(0x7000)), None);
        assert_eq!(map.get(page!(0x8000)), None);
        check_unrelated!(map);

        // remove all, start before, end after
        let mut map = PageMap::new();
        insert_unrelated!(map);
        map.insert_iter(page!(0x5000), [5, 6, 7, 8]);
        map.remove_many(page!(0x4000), 6);
        assert_eq!(map.get(page!(0x5000)), None);
        assert_eq!(map.get(page!(0x6000)), None);
        assert_eq!(map.get(page!(0x7000)), None);
        assert_eq!(map.get(page!(0x8000)), None);
        check_unrelated!(map);

        // start after
        let mut map = PageMap::new();
        insert_unrelated!(map);
        map.insert_iter(page!(0x5000), [5, 6, 7, 8]);
        map.remove_many(page!(0x6000), 3);
        assert_eq!(map[page!(0x5000)], 5);
        assert_eq!(map.get(page!(0x6000)), None);
        assert_eq!(map.get(page!(0x7000)), None);
        assert_eq!(map.get(page!(0x8000)), None);
        check_unrelated!(map);

        // end before
        let mut map = PageMap::new();
        insert_unrelated!(map);
        map.insert_iter(page!(0x5000), [5, 6, 7, 8]);
        map.remove_many(page!(0x5000), 3);
        assert_eq!(map.get(page!(0x5000)), None);
        assert_eq!(map.get(page!(0x6000)), None);
        assert_eq!(map.get(page!(0x7000)), None);
        assert_eq!(map[page!(0x8000)], 8);
        check_unrelated!(map);

        // start after, end before
        let mut map = PageMap::new();
        insert_unrelated!(map);
        map.insert_iter(page!(0x5000), [5, 6, 7, 8]);
        map.remove_many(page!(0x6000), 2);
        assert_eq!(map[page!(0x5000)], 5);
        assert_eq!(map.get(page!(0x6000)), None);
        assert_eq!(map.get(page!(0x7000)), None);
        assert_eq!(map[page!(0x8000)], 8);
        check_unrelated!(map);

        // remove two chunks
        let mut map = PageMap::new();
        insert_unrelated!(map);
        map.insert_iter(page!(0x05000), [5, 6, 7, 8]);
        map.insert_iter(page!(0x15000), [15, 16, 17, 18]);
        map.remove_many(page!(0x5000), 100);
        assert_eq!(map.get(page!(0x5000)), None);
        assert_eq!(map.get(page!(0x6000)), None);
        assert_eq!(map.get(page!(0x7000)), None);
        assert_eq!(map.get(page!(0x8000)), None);
        assert_eq!(map.get(page!(0x15000)), None);
        assert_eq!(map.get(page!(0x16000)), None);
        assert_eq!(map.get(page!(0x17000)), None);
        assert_eq!(map.get(page!(0x18000)), None);
        check_unrelated!(map);
    }
}
