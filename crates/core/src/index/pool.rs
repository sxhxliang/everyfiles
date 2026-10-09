//! Byte arena for names.
//!
//! Names are appended NUL-terminated into fixed-size blocks and never straddle
//! a block boundary (the writer pads to the next block instead). Both facts
//! are load-bearing for the query engine: a substring search can run over a
//! whole block at once, and because a query never contains `0x00`, a hit can
//! never span two adjacent names. Unused tail bytes of a block are zero, so
//! padding behaves like more NUL separators.

use std::sync::Arc;

use memmap2::Mmap;

use super::block::Block;
use crate::error::{Error, Result};

/// log2 of the block size in bytes.
pub const POOL_SHIFT: u32 = 20;
/// Block size in bytes (1 MiB).
pub const POOL_BLOCK: usize = 1 << POOL_SHIFT;
const MASK: usize = POOL_BLOCK - 1;

/// Append-only, block-stable byte arena.
#[derive(Debug, Default, Clone)]
pub struct NamePool {
    blocks: Vec<Block<u8>>,
    len: usize,
    _map: Option<Arc<Mmap>>,
}

impl NamePool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bytes used, including padding at block ends.
    #[inline]
    pub fn bytes_used(&self) -> usize {
        self.len
    }

    /// Heap bytes held by owned blocks.
    pub fn bytes_allocated(&self) -> usize {
        self.blocks.iter().filter(|b| b.is_owned()).count() * POOL_BLOCK
    }

    pub fn bytes_mapped(&self) -> usize {
        self.blocks.iter().filter(|b| !b.is_owned()).count() * POOL_BLOCK
    }

    #[inline]
    pub fn num_blocks(&self) -> usize {
        self.len.div_ceil(POOL_BLOCK)
    }

    /// Append `name` plus a NUL. Returns the offset of the first name byte.
    pub fn push(&mut self, name: &[u8]) -> Result<u32> {
        let need = name.len() + 1;
        if need > POOL_BLOCK {
            return Err(Error::NameTooLong(name.len()));
        }
        let within = self.len & MASK;
        if within + need > POOL_BLOCK {
            // Pad to the next block boundary so the name does not straddle.
            self.len = (self.len | MASK) + 1;
        }
        if self.len > u32::MAX as usize - need {
            return Err(Error::IndexFull);
        }
        let b = self.len >> POOL_SHIFT;
        while self.blocks.len() <= b {
            self.blocks.push(Block::owned_zeroed(POOL_BLOCK));
        }
        let w = self.len & MASK;
        let blk = self.blocks[b].as_mut();
        blk[w..w + name.len()].copy_from_slice(name);
        blk[w + name.len()] = 0;
        let off = self.len as u32;
        self.len += need;
        Ok(off)
    }

    /// The `len` bytes starting at `off`.
    #[inline]
    pub fn get(&self, off: u32, len: u16) -> &[u8] {
        let off = off as usize;
        let w = off & MASK;
        &self.blocks[off >> POOL_SHIFT][w..w + len as usize]
    }

    /// The populated prefix of block `b` (padding at the end is zero bytes).
    #[inline]
    pub fn block(&self, b: usize) -> &[u8] {
        let n = self.num_blocks();
        assert!(b < n, "block {b} out of range {n}");
        let used = if b + 1 == n {
            let r = self.len & MASK;
            if r == 0 { POOL_BLOCK } else { r }
        } else {
            POOL_BLOCK
        };
        &self.blocks[b][..used]
    }

    /// Byte offset at which block `b` starts.
    #[inline]
    pub fn block_base(b: usize) -> u32 {
        (b << POOL_SHIFT) as u32
    }

    /// Build from `len` contiguous bytes inside `map`. See
    /// [`super::Chunked::from_mapped`] for the contract.
    ///
    /// # Safety
    ///
    /// `ptr` must point to `len` bytes inside `map`, which must not be
    /// modified while this value exists.
    pub(crate) unsafe fn from_mapped(ptr: *const u8, len: usize, map: Arc<Mmap>) -> Self {
        let full = len / POOL_BLOCK;
        let rem = len % POOL_BLOCK;
        let mut blocks = Vec::with_capacity(full + usize::from(rem > 0));
        for b in 0..full {
            // SAFETY: within the caller's range.
            blocks.push(unsafe { Block::mapped(ptr.add(b * POOL_BLOCK), POOL_BLOCK) });
        }
        if rem > 0 {
            let mut tail = Block::owned_zeroed(POOL_BLOCK);
            // SAFETY: source `[full*POOL_BLOCK, len)` is within range; the
            // destination has room for POOL_BLOCK >= rem bytes.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    ptr.add(full * POOL_BLOCK),
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
    fn push_and_get_round_trip() {
        let mut p = NamePool::new();
        let a = p.push(b"alpha").unwrap();
        let b = p.push(b"").unwrap();
        let c = p.push("日本".as_bytes()).unwrap();
        assert_eq!(p.get(a, 5), b"alpha");
        assert_eq!(p.get(b, 0), b"");
        assert_eq!(p.get(c, 6), "日本".as_bytes());
        assert_eq!(p.block(0)[5], 0);
        assert_eq!(p.block(0)[6], 0);
    }

    #[test]
    fn name_never_straddles_a_block() {
        let mut p = NamePool::new();
        let filler = vec![b'x'; POOL_BLOCK - 3 - 1];
        let f = p.push(&filler).unwrap();
        assert_eq!(f, 0);
        assert_eq!(p.bytes_used(), POOL_BLOCK - 3);
        let n = p.push(b"hello").unwrap();
        assert_eq!(n as usize, POOL_BLOCK);
        assert_eq!(p.get(n, 5), b"hello");
        assert_eq!(p.num_blocks(), 2);
        assert!(p.block(0)[POOL_BLOCK - 3..].iter().all(|&b| b == 0));
    }

    #[test]
    fn exactly_fitting_name_does_not_pad() {
        let mut p = NamePool::new();
        let filler = vec![b'y'; POOL_BLOCK - 1];
        let off = p.push(&filler).unwrap();
        assert_eq!(off, 0);
        assert_eq!(p.bytes_used(), POOL_BLOCK);
        let next = p.push(b"z").unwrap();
        assert_eq!(next as usize, POOL_BLOCK);
    }

    #[test]
    fn oversized_name_is_rejected() {
        let mut p = NamePool::new();
        let huge = vec![b'q'; POOL_BLOCK];
        assert!(matches!(p.push(&huge), Err(Error::NameTooLong(_))));
    }

    #[test]
    fn existing_bytes_do_not_move_on_push() {
        let mut p = NamePool::new();
        let a = p.push(b"stable").unwrap();
        let ptr0 = p.get(a, 6).as_ptr();
        for _ in 0..3 {
            p.push(&vec![b'z'; POOL_BLOCK / 2]).unwrap();
        }
        assert_eq!(ptr0, p.get(a, 6).as_ptr());
    }
}
