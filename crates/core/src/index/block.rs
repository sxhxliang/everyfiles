//! One block of an index array: shared heap memory, or a view into a
//! read-only memory map kept alive by the containing structure.
//!
//! Owned blocks are `Arc<[T]>`, so cloning an array is a handful of
//! reference-count bumps and two indexes can share every block they have in
//! common. That is what makes publishing a read snapshot nearly free. Writes
//! go through [`Block::as_mut`], which gives the writer a private copy when
//! the block is shared or mapped (copy-on-write), so neither a live snapshot
//! nor the mapped file is ever written through.

use std::ops::Deref;
use std::sync::Arc;

pub(crate) enum Block<T> {
    Owned(Arc<[T]>),
    Mapped { ptr: *const T, len: usize },
}

impl<T> std::fmt::Debug for Block<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Owned(b) => write!(
                f,
                "Block::Owned(len={}, shared={})",
                b.len(),
                Arc::strong_count(b) > 1
            ),
            Self::Mapped { len, .. } => write!(f, "Block::Mapped(len={len})"),
        }
    }
}

impl<T> Clone for Block<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Owned(a) => Self::Owned(Arc::clone(a)),
            Self::Mapped { ptr, len } => Self::Mapped {
                ptr: *ptr,
                len: *len,
            },
        }
    }
}

impl<T: Copy + Default> Block<T> {
    pub fn owned_zeroed(n: usize) -> Self {
        Self::Owned(vec![T::default(); n].into())
    }

    /// View `len` elements starting at `ptr`.
    ///
    /// # Safety
    ///
    /// `ptr` must be properly aligned for `T` and point to `len` initialized
    /// `T`s that stay valid, mapped, and unmodified for as long as this block
    /// (and every clone of it) exists. The owner keeps the mapping alive.
    pub unsafe fn mapped(ptr: *const T, len: usize) -> Self {
        Self::Mapped { ptr, len }
    }

    pub fn is_owned(&self) -> bool {
        matches!(self, Self::Owned(_))
    }

    /// Mutable access with copy-on-write: a mapped block, or an owned block
    /// shared with a snapshot, is first copied into private memory.
    pub fn as_mut(&mut self) -> &mut [T] {
        let shared = match self {
            Self::Mapped { .. } => true,
            Self::Owned(a) => Arc::get_mut(a).is_none(),
        };
        if shared {
            let copy: Arc<[T]> = Arc::from(self.deref().to_vec());
            *self = Self::Owned(copy);
        }
        match self {
            Self::Owned(a) => Arc::get_mut(a).expect("unique after copy-on-write"),
            Self::Mapped { .. } => unreachable!("replaced above"),
        }
    }
}

impl<T> Deref for Block<T> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &[T] {
        match self {
            Self::Owned(b) => b,
            // SAFETY: `mapped` requires the caller to guarantee `ptr`/`len`
            // describe initialized, aligned, live memory that is never
            // modified while any clone of this block exists.
            Self::Mapped { ptr, len } => unsafe { std::slice::from_raw_parts(*ptr, *len) },
        }
    }
}

// SAFETY: a `Mapped` block points into a read-only memory map that is never
// mutated while the block exists (writes copy first), so sharing it across
// threads is sound whenever `T` itself is.
unsafe impl<T: Send> Send for Block<T> {}
// SAFETY: see above.
unsafe impl<T: Sync> Sync for Block<T> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_on_write_upgrades_mapped_to_owned() {
        let backing: Vec<u32> = (0..10).collect();
        // SAFETY: `backing` outlives `b` and is not mutated while `b` exists.
        let mut b = unsafe { Block::mapped(backing.as_ptr(), backing.len()) };
        assert!(!b.is_owned());
        b.as_mut()[0] = 99;
        assert!(b.is_owned());
        assert_eq!(b[0], 99);
        assert_eq!(backing[0], 0, "the mapping is untouched");
    }

    #[test]
    fn shared_owned_block_is_copied_before_write() {
        let mut a = Block::<u8>::owned_zeroed(4);
        a.as_mut()[0] = 1;
        let snapshot = a.clone();
        a.as_mut()[0] = 2; // must not affect `snapshot`
        assert_eq!(snapshot[0], 1);
        assert_eq!(a[0], 2);
        // An unshared block is written in place (no copy).
        let p0 = a.as_ptr();
        a.as_mut()[1] = 7;
        assert_eq!(p0, a.as_ptr());
    }
}
