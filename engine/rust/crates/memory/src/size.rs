//! A byte count with readable units, for budgets, limits and reports.

use core::fmt;

/// A number of bytes.
///
/// `ByteSize` is for quantities people read and configure: budgets, limits, statistics. It wraps a `u64` (the type
/// the `platform` crate reports total memory in) and offers only checked or saturating arithmetic, so no operation
/// can panic or wrap. Sizes used to place data in memory are plain `usize` values, see [`Alignment`].
///
/// ```
/// use memory::ByteSize;
///
/// let budget = ByteSize::from_mib(256);
/// assert_eq!(budget.bytes(), 268_435_456);
/// assert_eq!(budget.to_string(), "256.0 MiB");
/// assert_eq!(budget.checked_sub(ByteSize::from_mib(300)), None);
/// ```
///
/// [`Alignment`]: crate::Alignment
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ByteSize(u64);

impl ByteSize {
    /// Bytes in a kibibyte (1024).
    pub const KIB: u64 = 1 << 10;
    /// Bytes in a mebibyte (1024 KiB).
    pub const MIB: u64 = 1 << 20;
    /// Bytes in a gibibyte (1024 MiB).
    pub const GIB: u64 = 1 << 30;
    /// Bytes in a tebibyte (1024 GiB).
    pub const TIB: u64 = 1 << 40;

    /// Zero bytes.
    pub const ZERO: Self = Self(0);

    /// The largest representable size.
    pub const MAX: Self = Self(u64::MAX);

    /// A size of exactly `bytes` bytes.
    pub const fn from_bytes(bytes: u64) -> Self {
        Self(bytes)
    }

    /// `kib` kibibytes; saturates at [`ByteSize::MAX`] instead of overflowing.
    pub const fn from_kib(kib: u64) -> Self {
        Self(kib.saturating_mul(Self::KIB))
    }

    /// `mib` mebibytes; saturates at [`ByteSize::MAX`] instead of overflowing.
    pub const fn from_mib(mib: u64) -> Self {
        Self(mib.saturating_mul(Self::MIB))
    }

    /// `gib` gibibytes; saturates at [`ByteSize::MAX`] instead of overflowing.
    pub const fn from_gib(gib: u64) -> Self {
        Self(gib.saturating_mul(Self::GIB))
    }

    /// The size in bytes.
    pub const fn bytes(self) -> u64 {
        self.0
    }

    /// The size as a `usize`, or `None` when it does not fit (a size above 4 GiB on a 32-bit target).
    pub fn as_usize(self) -> Option<usize> {
        usize::try_from(self.0).ok()
    }

    /// Whether the size is zero.
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// The sum, or `None` on overflow.
    pub const fn checked_add(self, rhs: Self) -> Option<Self> {
        match self.0.checked_add(rhs.0) {
            Some(sum) => Some(Self(sum)),
            None => None,
        }
    }

    /// The difference, or `None` when `rhs` is larger than `self`.
    pub const fn checked_sub(self, rhs: Self) -> Option<Self> {
        match self.0.checked_sub(rhs.0) {
            Some(difference) => Some(Self(difference)),
            None => None,
        }
    }

    /// The sum, capped at [`ByteSize::MAX`].
    pub const fn saturating_add(self, rhs: Self) -> Self {
        Self(self.0.saturating_add(rhs.0))
    }

    /// The difference, floored at zero.
    pub const fn saturating_sub(self, rhs: Self) -> Self {
        Self(self.0.saturating_sub(rhs.0))
    }
}

impl From<u64> for ByteSize {
    fn from(bytes: u64) -> Self {
        Self(bytes)
    }
}

impl From<usize> for ByteSize {
    fn from(bytes: usize) -> Self {
        // `usize` is at most 64 bits on every supported target; the fallback only exists to stay panic-free.
        Self(u64::try_from(bytes).unwrap_or(u64::MAX))
    }
}

impl From<ByteSize> for u64 {
    fn from(size: ByteSize) -> Self {
        size.0
    }
}

/// Prints `512 B`, `1.5 KiB`, `3.0 MiB`, ... (binary units, one decimal place above 1 KiB).
impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const UNITS: [&str; 6] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];

        if self.0 < Self::KIB {
            return write!(f, "{} B", self.0);
        }

        // Display only: the conversion to f64 may round for sizes above 2^53, which one decimal place hides.
        let mut value = self.0 as f64 / 1024.0;
        let mut unit = 0;
        // 1023.95 prints as "1024.0" with one decimal, so it is promoted to the next unit instead.
        while value >= 1023.95 && unit + 1 < UNITS.len() {
            value /= 1024.0;
            unit += 1;
        }
        write!(f, "{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_constants_are_powers_of_1024() {
        assert_eq!(ByteSize::KIB, 1024);
        assert_eq!(ByteSize::MIB, 1024 * 1024);
        assert_eq!(ByteSize::GIB, 1024 * 1024 * 1024);
        assert_eq!(ByteSize::TIB, 1024 * 1024 * 1024 * 1024);
    }

    #[test]
    fn constructors_scale_by_the_unit() {
        assert_eq!(ByteSize::from_bytes(7).bytes(), 7);
        assert_eq!(ByteSize::from_kib(3).bytes(), 3 * 1024);
        assert_eq!(ByteSize::from_mib(2).bytes(), 2 * 1024 * 1024);
        assert_eq!(ByteSize::from_gib(1).bytes(), 1 << 30);
        assert!(ByteSize::ZERO.is_zero());
        assert!(!ByteSize::from_bytes(1).is_zero());
        assert_eq!(ByteSize::default(), ByteSize::ZERO);
    }

    #[test]
    fn constructors_saturate_instead_of_overflowing() {
        assert_eq!(ByteSize::from_kib(u64::MAX), ByteSize::MAX);
        assert_eq!(ByteSize::from_mib(u64::MAX), ByteSize::MAX);
        assert_eq!(ByteSize::from_gib(u64::MAX), ByteSize::MAX);
    }

    #[test]
    fn checked_arithmetic_reports_failure() {
        let one = ByteSize::from_bytes(1);
        let two = ByteSize::from_bytes(2);
        assert_eq!(one.checked_add(two), Some(ByteSize::from_bytes(3)));
        assert_eq!(ByteSize::MAX.checked_add(one), None);
        assert_eq!(two.checked_sub(one), Some(one));
        assert_eq!(one.checked_sub(two), None);
        assert_eq!(one.checked_sub(one), Some(ByteSize::ZERO));
    }

    #[test]
    fn saturating_arithmetic_clamps() {
        let one = ByteSize::from_bytes(1);
        assert_eq!(ByteSize::MAX.saturating_add(one), ByteSize::MAX);
        assert_eq!(ByteSize::ZERO.saturating_sub(one), ByteSize::ZERO);
        assert_eq!(
            ByteSize::from_bytes(5).saturating_sub(ByteSize::from_bytes(2)),
            ByteSize::from_bytes(3)
        );
    }

    #[test]
    fn converts_to_and_from_integers() {
        assert_eq!(ByteSize::from(42u64).bytes(), 42);
        assert_eq!(ByteSize::from(42usize).bytes(), 42);
        assert_eq!(u64::from(ByteSize::from_kib(1)), 1024);
        assert_eq!(ByteSize::from_bytes(42).as_usize(), Some(42));
    }

    #[test]
    fn orders_by_byte_count() {
        assert!(ByteSize::from_kib(1) < ByteSize::from_mib(1));
        assert!(ByteSize::MAX > ByteSize::from_gib(1));
    }

    #[test]
    fn displays_binary_units() {
        let cases: [(u64, &str); 12] = [
            (0, "0 B"),
            (1, "1 B"),
            (1023, "1023 B"),
            (1024, "1.0 KiB"),
            (1536, "1.5 KiB"),
            (ByteSize::MIB - 1, "1.0 MiB"),
            (ByteSize::MIB, "1.0 MiB"),
            (5 * ByteSize::MIB, "5.0 MiB"),
            (ByteSize::GIB, "1.0 GiB"),
            (3 * ByteSize::GIB / 2, "1.5 GiB"),
            (ByteSize::TIB, "1.0 TiB"),
            (u64::MAX, "16.0 EiB"),
        ];
        for (bytes, expected) in cases {
            assert_eq!(ByteSize::from_bytes(bytes).to_string(), expected, "{bytes}");
        }
    }
}
