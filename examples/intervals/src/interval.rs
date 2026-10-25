//  examples/intervals/src/interval.rs

//! Closed integer intervals.

/// The closed interval `[lo, hi]`.
#[derive(Clone, Copy, Debug)]
pub struct Interval {
    pub lo: i64,
    pub hi: i64,
}

/// Whether two closed intervals share at least one point.
///
/// Both ends are included, so intervals that only touch overlap. That one
/// comparison is the argument the example's mutation table puts to the test.
pub fn overlaps(a: Interval, b: Interval) -> bool {
    a.lo <= b.hi && b.lo <= a.hi
}

#[cfg(test)]
mod tests {
    use super::{Interval, overlaps};

    fn iv(lo: i64, hi: i64) -> Interval {
        Interval { lo, hi }
    }

    #[test]
    fn touching_intervals_overlap() {
        assert!(overlaps(iv(0, 1), iv(1, 2)));
    }

    #[test]
    fn adjacent_intervals_do_not() {
        assert!(!overlaps(iv(0, 1), iv(2, 3)));
    }

    #[test]
    fn disjoint_intervals_do_not() {
        assert!(!overlaps(iv(0, 1), iv(3, 4)));
    }

    #[test]
    fn nested_intervals_overlap() {
        assert!(overlaps(iv(0, 10), iv(2, 3)));
    }

    #[test]
    fn overlap_is_symmetric() {
        let pairs = [
            (iv(0, 1), iv(1, 2)),
            (iv(0, 1), iv(2, 3)),
            (iv(0, 1), iv(3, 4)),
            (iv(0, 10), iv(2, 3)),
        ];
        for (a, b) in pairs {
            assert_eq!(overlaps(a, b), overlaps(b, a), "{a:?} and {b:?}");
        }
    }
}
