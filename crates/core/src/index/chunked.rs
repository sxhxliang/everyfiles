//! A growable array whose existing elements never move.
//!
//! Elements live in fixed-size blocks (see [`Block`]). Appending may
//! reallocate the *vector of blocks* but never a block, so a reference into a
//! published block stays valid for as long as the block is alive. That is the
//! property the lock-free snapshot design depends on, and it is also what lets
//! a loaded index keep every full block as a zero-copy view into the file.

use std::sync::Arc;

use memmap2::Mmap;

use super::block::Block;

/// log2 of the block size.
pub const BLOCK_SHIFT: u32 = 16;
/// Elements per block.
pub const BLOCK: usize = 1 << BLOCK_SHIFT;
const MASK: usize = BLOCK - 1;

/// A chunked, append-only vector of `Copy` values.
#[derive(Debug, Clone)]
pub struct Chunked<T> {
    blocks: Vec<Block<T>>,
    len: usize,
    /// Keeps a memory map alive while any block views into it.
    _map: Option<Arc<Mmap>>,
}

impl<T: Copy + Default> Default for Chunked<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy + Default> Chunked<T> {
    pub fn new() -> Self {
        Self {
            blocks: Vec::new(),
            len: 0,
            _map: None,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Heap bytes held by owned blocks (mapped blocks live in the file cache).
    pub fn bytes_allocated(&self) -> usize {
        self.blocks.iter().filter(|b| b.is_owned()).count() * BLOCK * size_of::<T>()
    }

    /// Bytes of mapped blocks.
    pub fn bytes_mapped(&self) -> usize {
        self.blocks.iter().filter(|b| !b.is_owned()).count() * BLOCK * size_of::<T>()
    }

    pub fn push(&mut self, v: T) {
        if self.len & MASK == 0 && self.len >> BLOCK_SHIFT == self.blocks.len() {
            self.blocks.push(Block::owned_zeroed(BLOCK));
        }
        let b = self.len >> BLOCK_SHIFT;
        let w = self.len & MASK;
        self.blocks[b].as_mut()[w] = v;
        self.len += 1;
    }

    /// Panics if `i >= len`.
    #[inline]
    pub fn get(&self, i: usize) -> T {
        assert!(i < self.len, "index {i} out of range {}", self.len);
        self.blocks[i >> BLOCK_SHIFT][i & MASK]
    }

    /// Panics if `i >= len`. A mapped block is copied before being written.
    #[inline]
    pub fn set(&mut self, i: usize, v: T) {
        assert!(i < self.len, "index {i} out of range {}", self.len);
        self.blocks[i >> BLOCK_SHIFT].as_mut()[i & MASK] = v;
    }

    /// Number of blocks that hold at least one element.
    #[inline]
    pub fn num_blocks(&self) -> usize {
        self.len.div_ceil(BLOCK)
    }

    /// The populated prefix of block `b`.
    #[inline]
    pub fn block(&self, b: usize) -> &[T] {
        let n = self.num_blocks();
        assert!(b < n, "block {b} out of range {n}");
        let used = if b + 1 == n {
            let r = self.len & MASK;
            if r == 0 { BLOCK } else { r }
        } else {
            BLOCK
        };
        &self.blocks[b][..used]
    }

    /// Iterate the populated prefix of every block.
    pub fn blocks(&self) -> impl Iterator<Item = &[T]> + '_ {
        (0..self.num_blocks()).map(move |b| self.block(b))
    }

    /// Build from `len` contiguous elements inside `map`. Full blocks become
    /// zero-copy views; a partial tail block is copied so it can grow.
    ///
    /// # Safety
    ///
    /// `ptr` must be aligned for `T` and point to `len` initialized `T`s that
    /// lie inside `map`, which must not be modified while this value exists.
    pub(crate) unsafe fn from_mapped(ptr: *const T, len: usize, map: Arc<Mmap>) -> Self {
        let full = len / BLOCK;
        let rem = len % BLOCK;
        let mut blocks = Vec::with_capacity(full + usize::from(rem > 0));
        for b in 0..full {
            // SAFETY: `b * BLOCK + BLOCK <= len`, within the caller's range.
            blocks.push(unsafe { Block::mapped(ptr.add(b * BLOCK), BLOCK) });
        }
        if rem > 0 {
            let mut tail = Block::owned_zeroed(BLOCK);
            // SAFETY: source range `[full*BLOCK, len)` is within the caller's
            // range; destination has capacity BLOCK >= rem; non-overlapping.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    ptr.add(full * BLOCK),
                    tail.as_mut().as_mut_ptr(),
                    rem,
                );
            }
            blocks.push(tail);
        }
        Self {
            blocks,
            len,
            _map: Some(map),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_get_set_across_blocks() {
        let mut c = Chunked::<u32>::new();
        let n = BLOCK * 3 + 5;
        for i in 0..n {
            c.push(i as u32);
        }
        assert_eq!(c.len(), n);
        assert_eq!(c.num_blocks(), 4);
        for i in [0, 1, BLOCK - 1, BLOCK, BLOCK + 1, n - 1] {
            assert_eq!(c.get(i), i as u32);
        }
        c.set(BLOCK, 99);
        assert_eq!(c.get(BLOCK), 99);
    }

    #[test]
    fn block_views_cover_exactly_len() {
        let mut c = Chunked::<u8>::new();
        for i in 0..(BLOCK + 3) {
            c.push((i % 251) as u8);
        }
        let total: usize = c.blocks().map(<[u8]>::len).sum();
        assert_eq!(total, BLOCK + 3);
        assert_eq!(c.block(0).len(), BLOCK);
        assert_eq!(c.block(1).len(), 3);
    }

    #[test]
    fn exact_multiple_has_full_last_block() {
        let mut c = Chunked::<u16>::new();
        for i in 0..(BLOCK * 2) {
            c.push(i as u16);
        }
        assert_eq!(c.num_blocks(), 2);
        assert_eq!(c.block(1).len(), BLOCK);
    }

    #[test]
    fn existing_elements_do_not_move_on_push() {
        let mut c = Chunked::<u64>::new();
        c.push(7);
        let p0 = std::ptr::from_ref(&c.block(0)[0]);
        for i in 0..(BLOCK * 2) {
            c.push(i as u64);
        }
        let p1 = std::ptr::from_ref(&c.block(0)[0]);
        assert_eq!(p0, p1, "first block was reallocated");
        assert_eq!(c.get(0), 7);
    }
}
