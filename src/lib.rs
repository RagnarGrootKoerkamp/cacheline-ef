//! # Cacheline Elias-Fano
//!
//! [`CachelineEf`] is an integer encoding that packs chunks of 44 sorted 40-bit values into a single
//! cacheline, using `64/44*8 = 11.6` bits per value.
//! Each chunk can hold increasing values in a range of length `256*84=21504`.
//! If this range is exceeded, [`CachelineEfVec::new`] will panic while [`CachelineEf::try_new`] will return `None`.
//!
//! [`CachelineEfVec`] stores a vector of [`CachelineEf`] and provides [`CachelineEfVec::index`] and [`CachelineEfVec::prefetch`].
//!
//! This encoding is efficient when consecutive values differ by roughly 100, where using
//! Elias-Fano directly on the full list would use around `9` bits/value.
//!
//! The main benefit is that CachelineEf only requires reading a single cacheline per
//! query, where Elias-Fano encoding usually needs 3 reads.
//!
//! Epserde is supported when the `epserde` feature flag is enabled.
//!
//! ## Layout
//!
//! The layout is described in detail in the PtrHash paper ([arXiv](https://arxiv.org/abs/2502.15539),
//! [blog version](https://curiouscoding.nl/posts/ptrhash)).
//!
//! In summary:
//! - First store a 4 byte offset, corresponding to the 32 high bits of the smallest value.
//! - Then store for each of 44 values the 8 low bits.
//! - Lastly we have 16 bytes (128 bits) to encode the high parts.
//!   For the i'th value `x[i]`, we set the bit at position `i+(x[i]/256 - x[0]/256)` to `1`.

use common_traits::SelectInWord;
use std::cmp::min;

/// Number of stored values per unit.
const L: usize = 44;

/// A vector of [`CachelineEf`].
#[derive(Default, Clone, mem_dbg::MemSize, mem_dbg::MemDbg)]
#[cfg_attr(feature = "epserde", derive(epserde::prelude::Epserde))]
pub struct CachelineEfVec<E = Vec<CachelineEf>> {
    ef: E,
    len: usize,
}

impl CachelineEfVec<Vec<CachelineEf>> {
    /// Construct a new `CachelineEfVec` for a list of `u64` values.
    ///
    /// Panics when:
    /// - the input is not sorted,
    /// - the input values are over 2^40,
    /// - there is a cacheline where the values span a too large range.
    pub fn try_new(vals: &[u64]) -> Option<Self> {
        let mut p = Vec::with_capacity(vals.len().div_ceil(L));
        for i in (0..vals.len()).step_by(L) {
            p.push(CachelineEf::try_new(&vals[i..min(i + L, vals.len())])?);
        }

        Some(Self {
            ef: p,
            len: vals.len(),
        })
    }

    /// Construct a new `CachelineEfVec` for a list of `u64` values.
    ///
    /// Panics when:
    /// - the input is not sorted,
    /// - the input values are over 2^40,
    /// - there is a cacheline where the values span a too large range.
    pub fn new(vals: &[u64]) -> Self {
        Self::try_new(vals).expect("Values are too sparse!")
    }
}

impl<E: AsRef<[CachelineEf]>> CachelineEfVec<E> {
    /// Get the value at the given index in the vector.
    pub fn index(&self, index: usize) -> u64 {
        assert!(
            index < self.len,
            "Index {index} out of bounds. Length is {}.",
            self.len
        );
        // Note: This division is inlined by the compiler.
        unsafe { self.ef.as_ref().get_unchecked(index / L).index(index % L) }
    }
    /// The number of values stored.
    pub fn len(&self) -> usize {
        self.len
    }
    /// Get the value at the given index in the vector, and do not check bounds.
    pub unsafe fn index_unchecked(&self, index: usize) -> u64 {
        // Note: This division is inlined by the compiler.
        (*self.ef.as_ref().get_unchecked(index / L)).index(index % L)
    }
    /// Prefetch the cacheline containing the given element.
    pub fn prefetch(&self, index: usize) {
        prefetch_index(self.ef.as_ref(), index / L);
    }
    /// The size of the underlying vector, in bytes.
    pub fn size_in_bytes(&self) -> usize {
        std::mem::size_of_val(self.ef.as_ref())
    }
}

/// A single cacheline that holds 44 Elias-Fano encoded 40-bit values in a range of size `256*84=21504`.
// This has size 64 bytes (one cacheline) and is aligned to 64bytes as well to
// ensure it actually occupied a single cacheline.
// It is marked `zero_copy` to be able to use it with lazy deserialization of ep-serde.
#[derive(Clone, Copy, mem_dbg::MemSize, mem_dbg::MemDbg)]
#[repr(C)]
#[repr(align(64))]
#[cfg_attr(feature = "epserde", derive(epserde::prelude::Epserde))]
#[cfg_attr(feature = "epserde", zero_copy)]
#[copy_type]
pub struct CachelineEf {
    /// 2*64 = 128 bits to indicate where 256 boundaries are crossed.
    /// There are 44 1-bits corresponding to the stored numbers, and the number
    /// of 0-bits before each number indicates the number of times 256 must be added.
    high_boundaries: [u64; 2],
    /// The offset of the first element, divided by 256.
    reduced_offset: u32,
    /// Last 8 bits of each number.
    low_bits: [u8; L],
}

impl CachelineEf {
    fn try_new(vals: &[u64]) -> Option<Self> {
        assert!(!vals.is_empty(), "List of values must not be empty.");
        assert!(
            vals.len() <= L,
            "Number of values must be at most {L}, but is {}",
            vals.len()
        );
        let l = vals.len();
        if vals[l - 1] - vals[0] > 256 * (128 - L as u64) {
            return None;
        }
        // assert!(
        //     vals[l - 1] - vals[0] <= 256 * (128 - L as u64),
        //     "Range of values {} ({} to {}) is too large! Can be at most {}.",
        //     vals[l - 1] - vals[0],
        //     vals[0],
        //     vals[l - 1],
        //     256 * (128 - L as u64)
        // );
        assert!(
            vals[l - 1] < (1u64 << 40),
            "Last value {} is too large! Must be less than 2^40={}",
            vals[l - 1],
            1u64 << 40
        );

        let offset = vals[0] >> 8;
        assert!(
            offset <= u32::MAX as u64,
            "vals[0] does not fit in 40 bits."
        );
        let mut low_bits = [0u8; L];
        for (i, &v) in vals.iter().enumerate() {
            low_bits[i] = (v & 0xff) as u8;
        }
        let mut high_boundaries = [0u64; 2];
        let mut last = 0;
        for (i, &v) in vals.iter().enumerate() {
            assert!(i >= last, "Values are not sorted! {last} > {i}");
            last = i;
            let idx = i + ((v >> 8) - offset) as usize;
            assert!(idx < 128, "Value {} is too large!", v - offset);
            high_boundaries[idx / 64] |= 1 << (idx % 64);
        }
        Some(Self {
            reduced_offset: offset as u32,
            high_boundaries,
            low_bits,
        })
    }

    /// Get the value a the given index.
    ///
    /// Panics when `idx` is out of bounds.
    pub fn index(&self, idx: usize) -> u64 {
        let p = self.high_boundaries[0].count_ones() as usize;
        let one_pos = if idx < p {
            self.high_boundaries[0].select_in_word(idx)
        } else {
            64 + self.high_boundaries[1].select_in_word(idx - p)
        };

        256 * self.reduced_offset as u64 + 256 * (one_pos - idx) as u64 + self.low_bits[idx] as u64
    }
}

/// Prefetch the given cacheline into L1 cache.
fn prefetch_index<T>(s: &[T], index: usize) {
    let ptr = unsafe { s.as_ptr().add(index) as *const u64 };
    #[cfg(target_arch = "x86_64")]
    unsafe {
        std::arch::x86_64::_mm_prefetch(ptr as *const i8, std::arch::x86_64::_MM_HINT_T0);
    }
    #[cfg(target_arch = "x86")]
    unsafe {
        std::arch::x86::_mm_prefetch(ptr as *const i8, std::arch::x86::_MM_HINT_T0);
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        // TODO: Put this behind a feature flag.
        // std::arch::aarch64::_prefetch(ptr as *const i8, std::arch::aarch64::_PREFETCH_LOCALITY3);
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
    {
        // Do nothing.
    }
}

#[test]
fn test() {
    let max = (128 - L) * 256;
    let offset = rand::random::<u64>() % (1 << 40);
    let mut vals = [0u64; L];
    for _ in 0..1000000 {
        for v in &mut vals {
            *v = offset + rand::random::<u64>() % max as u64;
        }
        vals.sort_unstable();

        let lef = CachelineEf::try_new(&vals).unwrap();
        for i in 0..L {
            assert_eq!(lef.index(i), vals[i], "error; full list: {:?}", vals);
        }
    }
}

#[test]
fn size() {
    assert_eq!(std::mem::size_of::<CachelineEf>(), 64);
}
