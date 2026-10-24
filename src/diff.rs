use crate::list::{push_op, ListOp};
use crate::map::MapOp;
use std::collections::BTreeMap;

/// Above this size (n*m for the changed middle part) the LCS table gets too
/// big. The whole middle part is then replaced with a single splice instead.
const LCS_LIMIT: usize = 1 << 20;

/// Computes a minimal sequence of inserts and removes that turns `old` into
/// `new`. The common start and end are skipped first, so common cases like
/// push, pop or removing one element are O(n). Only the changed middle part
/// is compared with LCS (longest common subsequence). Adjacent ops are
/// merged by `push_op`.
pub(crate) fn list_diff<T: Clone + PartialEq>(old: &[T], new: &[T], out: &mut Vec<ListOp<T>>) {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let (old, new) = (&old[prefix..], &new[prefix..]);
    let suffix = old.iter().rev().zip(new.iter().rev()).take_while(|(a, b)| a == b).count();
    let (a, b) = (&old[..old.len() - suffix], &new[..new.len() - suffix]);
    let (n, m) = (a.len(), b.len());

    if n.saturating_mul(m) > LCS_LIMIT {
        push_op(out, ListOp::splice(prefix, n, b.to_vec()));
        return;
    }

    // lcs[i * w + j] = longest common subsequence of a[i..] and b[j..].
    // One flat allocation; u32 is enough since n*m is at most LCS_LIMIT.
    let w = m + 1;
    let mut lcs = vec![0u32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i * w + j] =
                if a[i] == b[j] { lcs[(i + 1) * w + j + 1] + 1 } else { lcs[(i + 1) * w + j].max(lcs[i * w + j + 1]) };
        }
    }

    let (mut i, mut j, mut pos) = (0, 0, prefix);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            i += 1;
            j += 1;
            pos += 1;
        } else if i < n && (j == m || lcs[(i + 1) * w + j] >= lcs[i * w + j + 1]) {
            push_op(out, ListOp::Remove(pos));
            i += 1;
        } else {
            push_op(out, ListOp::Insert(pos, b[j].clone()));
            j += 1;
            pos += 1;
        }
    }
}

/// Computes the inserts and removes that turn `old` into `new`, in key order
/// so the log comes out the same every time.
pub(crate) fn map_diff<K: Ord + Clone, V: Clone + PartialEq>(
    old: &BTreeMap<K, V>,
    new: &BTreeMap<K, V>,
    out: &mut Vec<MapOp<K, V>>,
) {
    out.extend(old.keys().filter(|k| !new.contains_key(k)).map(|k| MapOp::Remove(k.clone())));
    out.extend(new.iter().filter(|(k, v)| old.get(k) != Some(v)).map(|(k, v)| MapOp::Insert(k.clone(), v.clone())));
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 33) % n.max(1) as u64) as usize
        }
    }

    fn diff<T: Clone + PartialEq>(old: &[T], new: &[T]) -> Vec<ListOp<T>> {
        let mut ops = Vec::new();
        list_diff(old, new, &mut ops);
        ops
    }

    fn apply<T: Clone + PartialEq>(old: &[T], ops: &[ListOp<T>]) -> Vec<T> {
        let mut result = old.to_vec();
        for op in ops {
            op.apply_to(&mut result).unwrap();
        }
        result
    }

    /// Generates random lists, computes the diff and checks that it really
    /// turns the old list into the new one.
    #[test]
    fn list_diff_is_correct() {
        let mut rng = Rng(12345);
        for _ in 0..5000 {
            let old: Vec<u8> = (0..rng.below(8)).map(|_| rng.below(4) as u8).collect();
            let new: Vec<u8> = (0..rng.below(8)).map(|_| rng.below(4) as u8).collect();
            let ops = diff(&old, &new);
            assert_eq!(apply(&old, &ops), new, "old={old:?} ops={ops:?}");
        }
    }

    #[test]
    fn simple_list_changes_give_minimal_events() {
        assert_eq!(diff(&[1, 2, 3], &[1, 2, 3, 4]), [ListOp::Insert(3, 4)]);
        assert_eq!(diff(&[1, 2, 3], &[1, 3]), [ListOp::Remove(1)]);
        assert_eq!(diff(&[1, 2, 3, 4], &[2, 3]), [ListOp::Remove(0), ListOp::Remove(2)]);
        assert_eq!(diff(&[1, 2, 3], &[1, 9, 3]), [ListOp::Set(1, 9)]);
        assert_eq!(diff(&[1, 2, 3, 4], &[1, 8, 9, 4]), [ListOp::splice(1, 2, vec![8, 9])]);
    }

    #[test]
    fn map_diff_is_correct() {
        let mut rng = Rng(4242);
        let random_map = |rng: &mut Rng| -> BTreeMap<u8, u8> {
            (0..rng.below(8)).map(|_| (rng.below(6) as u8, rng.below(3) as u8)).collect()
        };
        for _ in 0..5000 {
            let old = random_map(&mut rng);
            let new = random_map(&mut rng);
            let mut ops = Vec::new();
            map_diff(&old, &new, &mut ops);
            let mut result = old.clone();
            for op in &ops {
                op.apply_to(&mut result).unwrap();
            }
            assert_eq!(result, new, "old={old:?} ops={ops:?}");
        }
    }

    /// Large changes (like sorting a long list) shouldn't build an LCS table
    /// with n*m entries, and should be logged as one splice.
    #[test]
    fn list_diff_handles_large_lists() {
        let old: Vec<u32> = (0..5000).collect();
        let new: Vec<u32> = old.iter().rev().copied().collect();
        let ops = diff(&old, &new);
        assert_eq!(ops.len(), 1);
        assert_eq!(apply(&old, &ops), new);
    }
}
