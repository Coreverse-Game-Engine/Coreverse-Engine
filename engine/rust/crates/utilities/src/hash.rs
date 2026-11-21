//! FNV-1a hashes of bytes, computable at compile time.
//!
//! FNV-1a is a fast, simple, non-cryptographic hash with a fixed definition, so the same bytes give the same
//! number on every machine, in every build and in every language that implements it. That makes it fit for ids
//! derived from names (`const PLAYER: u64 = fnv1a_64(b"player");`), which can then be compared, matched and
//! stored as plain integers. It is **not** a defence against an attacker choosing colliding input (do not use it
//! for a `HashMap` that takes untrusted keys, or for anything that needs a secure hash), and its output is a
//! different thing from `std::hash::Hash`, which is allowed to change between Rust versions.

const FNV32_OFFSET: u32 = 0x811c_9dc5;
const FNV32_PRIME: u32 = 0x0100_0193;
const FNV64_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV64_PRIME: u64 = 0x0000_0100_0000_01b3;

/// The 32-bit FNV-1a hash of `bytes`. The hash of no bytes is the offset basis, `0x811c9dc5`.
///
/// ```
/// use utilities::fnv1a_32;
///
/// const ID: u32 = fnv1a_32(b"a");
/// assert_eq!(ID, 0xe40c_292c);
/// ```
#[must_use]
pub const fn fnv1a_32(bytes: &[u8]) -> u32 {
    let mut hash = FNV32_OFFSET;
    let mut index = 0;
    while index < bytes.len() {
        hash ^= bytes[index] as u32;
        hash = hash.wrapping_mul(FNV32_PRIME);
        index += 1;
    }
    hash
}

/// The 64-bit FNV-1a hash of `bytes`. The hash of no bytes is the offset basis, `0xcbf29ce484222325`.
///
/// ```
/// use utilities::fnv1a_64;
///
/// const PLAYER: u64 = fnv1a_64(b"player");
/// assert_eq!(PLAYER, fnv1a_64("player".as_bytes()));
/// assert_ne!(PLAYER, fnv1a_64(b"enemy"));
/// ```
#[must_use]
pub const fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash = FNV64_OFFSET;
    let mut index = 0;
    while index < bytes.len() {
        hash ^= bytes[index] as u64;
        hash = hash.wrapping_mul(FNV64_PRIME);
        index += 1;
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_published_32_bit_vectors() {
        assert_eq!(fnv1a_32(b""), 0x811c_9dc5);
        assert_eq!(fnv1a_32(b"a"), 0xe40c_292c);
        assert_eq!(fnv1a_32(b"foobar"), 0xbf9c_f968);
    }

    #[test]
    fn matches_the_published_64_bit_vectors() {
        assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn works_in_a_const_context() {
        const ID32: u32 = fnv1a_32(b"player");
        const ID64: u64 = fnv1a_64(b"player");
        assert_eq!(ID32, 0x2c99_c300);
        assert_eq!(ID64, 0x4580_fab0_3b7e_b9c0);
    }

    #[test]
    fn a_different_input_gives_a_different_hash() {
        assert_ne!(fnv1a_64(b"player"), fnv1a_64(b"Player"));
        assert_ne!(fnv1a_64(b"ab"), fnv1a_64(b"ba"));
    }
}
