use common_traits::SelectInWord;
use std::cmp::min;

/// Number of stored values per unit.
const L: usize = 44;

/// `CachelineEf` is an integer encoding that packs chunks of 44 40-bit values into a single
/// cacheline, using 64/44*8 = 11.6 bits per value.
/// Each chunk can hold increasing values in a range of length 256*84=21504.
///
/// This is efficient when consecutive values differ by roughly 100, where using
/// Elias-Fano directly on the full list would use around 9 bits/value.
///
/// The main benefit is that this only requires reading a single cacheline per
/// query, where Elias-Fano encoding usually needs 3 reads.
#[derive(Default, Clone, mem_dbg::MemSize, mem_dbg::MemDbg)]
#[cfg_attr(feature = "epserde", derive(epserde::prelude::Epserde))]
pub struct CachelineEfVec<E = Vec<CachelineEf>> {
    ef: E,
    len: usize,
}

impl CachelineEfVec<Vec<CachelineEf>> {
    pub fn new(vals: &[u64]) -> Self {
        let mut p = Vec::with_capacity(vals.len().div_ceil(L));
        for i in (0..vals.len()).step_by(L) {
            p.push(CachelineEf::new(&vals[i..min(i + L, vals.len())]));
        }

        Self {
            ef: p,
            len: vals.len(),
        }
    }
}

impl<E: AsRef<[CachelineEf]>> CachelineEfVec<E> {
    pub fn index(&self, index: usize) -> u64 {
        assert!(
            index < self.len,
            "Index {index} out of bounds. Length is {}.",
            self.len
        );
        // Note: This division is inlined by the compiler.
        unsafe { self.ef.as_ref().get_unchecked(index / L).get(index % L) }
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub unsafe fn index_unchecked(&self, index: usize) -> u64 {
        // Note: This division is inlined by the compiler.
        (*self.ef.as_ref().get_unchecked(index / L)).get(index % L)
    }
    pub fn prefetch(&self, index: usize) {
        prefetch_index(self.ef.as_ref(), index / L);
    }
    pub fn size_in_bytes(&self) -> usize {
        std::mem::size_of_val(self.ef.as_ref())
    }
}

/// Single-cacheline Elias-Fano encoding that holds 44 40-bit values in a range of size 256*84=21504.
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
    // 2*64 = 128 bits to indicate where 256 boundaries are crossed.
    // There are 44 1-bits corresponding to the stored numbers, and the number
    // of 0-bits before each number indicates the number of times 256 must be added.
    high_boundaries: [u64; 2],
    // The offset of the first element, divided by 256.
    reduced_offset: u32,
    // Last 8 bits of each number.
    low_bits: [u8; L],
}

impl CachelineEf {
    fn new(vals: &[u64]) -> Self {
        assert!(!vals.is_empty(), "List of values must not be empty.");
        assert!(
            vals.len() <= L,
            "Number of values must be at most {L}, but is {}",
            vals.len()
        );
        let l = vals.len();
        assert!(
            vals[l - 1] - vals[0] <= 256 * (128 - L as u64),
            "Range of values {} ({} to {}) is too large! Can be at most {}.",
            vals[l - 1] - vals[0],
            vals[0],
            vals[l - 1],
            256 * (128 - L as u64)
        );
        assert!(
            vals[l - 1] < (1 << 40),
            "Last value {} is too large! Must be less than 2^40={}",
            vals[l - 1],
            1 << 40
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
        for (i, &v) in vals.iter().enumerate() {
            let idx = i + ((v >> 8) - offset) as usize;
            assert!(idx < 128, "Value {} is too large!", v - offset);
            high_boundaries[idx / 64] |= 1 << (idx % 64);
        }
        Self {
            reduced_offset: offset as u32,
            high_boundaries,
            low_bits,
        }
    }

    fn get(&self, idx: usize) -> u64 {
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

        let lef = CachelineEf::new(&vals);
        for i in 0..L {
            assert_eq!(lef.get(i), vals[i], "error; full list: {:?}", vals);
        }
    }
}
