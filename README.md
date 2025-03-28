# Cacheline Elias-Fano

[![crates.io](https://img.shields.io/crates/v/cacheline-ef.svg)](https://crates.io/crates/cacheline-ef)
[![docs.rs](https://img.shields.io/docsrs/cacheline-ef.svg)](https://docs.rs/cacheline-ef)

`CachelineEf` is an integer encoding that packs chunks of 44 sorted 40-bit values into a single
cacheline, using `64/44*8 = 11.6` bits per value.
Each chunk can hold increasing values in a range of length `256*84=21504`.

`CachelineEfVec` stores a vector of `CachelineEf` and provides `CachelineEfVec::index` and `CachelineEfVec::prefetch`.

This encoding is efficient when consecutive values differ by roughly 100, where using
Elias-Fano directly on the full list would use around `9` bits/value.

The main benefit is that CachelineEf only requires reading a single cacheline per
query, where Elias-Fano encoding usually needs 3 reads.

Epserde is supported when the `epserde` feature flag is enabled.

## Layout

The layout is described in detail in the PtrHash paper ([arXiv](https://arxiv.org/abs/2502.15539),
[blog version](https://curiouscoding.nl/posts/ptrhash)).

In summary:
- First store a 4 byte offset, corresponding to the 32 high bits of the smallest value.
- Then store for each of 44 values the 8 low bits.
- Lastly we have 16 bytes (128 bits) to encode the high parts.
  For the i'th value `x[i]`, we set the bit at position `i+(x[i]/256 - x[0]/256)` to `1`.
