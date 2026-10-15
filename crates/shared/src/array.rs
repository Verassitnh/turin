//! Port of upstream/packages/shared/src/Array.ts

/// Sorts a copy of `values` with `compare` (JS `[...values].sort(compareFn)`).
///
/// JS `Array.prototype.sort` is stable; `sort_by` is too.
pub fn copy_sorted<T: Clone, F>(values: &[T], compare: F) -> Vec<T>
where
    F: FnMut(&T, &T) -> std::cmp::Ordering,
{
    let mut copy = values.to_vec();
    copy.sort_by(compare);
    copy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_a_copy_without_mutating_the_source() {
        let source = [3, 1, 2];
        assert_eq!(
            copy_sorted(&source, |left, right| left.cmp(right)),
            vec![1, 2, 3]
        );
        assert_eq!(source, [3, 1, 2]);
    }
}
