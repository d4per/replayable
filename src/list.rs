use crate::diff::list_diff;
use crate::field::{Field, Value};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Deref, DerefMut, Index, IndexMut};
use std::slice::SliceIndex;
use std::sync::atomic::{AtomicU64, Ordering};

/// A logged change to a [`List`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ListOp<T> {
    /// Insert a value at an index, like `Vec::insert`.
    Insert(usize, T),
    /// Remove the value at an index, like `Vec::remove`.
    Remove(usize),
    /// Replace the value at an index.
    Set(usize, T),
    /// Remove everything.
    Clear,
    /// Replace a range with other values, like `Vec::splice`. Used when
    /// several adjacent changes are made in one edit, for example by
    /// `extend`, `truncate` or a sort.
    ///
    /// Boxed so that the other, much more common ops stay small.
    Splice(Box<Splice<T>>),
}

/// The contents of [`ListOp::Splice`]: replace `remove` values starting at
/// `at` with `insert`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Splice<T> {
    /// Where the change starts.
    pub at: usize,
    /// How many values are removed from `at`.
    pub remove: usize,
    /// The values inserted at `at`.
    pub insert: Vec<T>,
}

impl<T> ListOp<T> {
    /// The op as `(at, removed, inserted)`, or `None` for `Clear`.
    fn span(&self) -> Option<(usize, usize, usize)> {
        match self {
            ListOp::Insert(i, _) => Some((*i, 0, 1)),
            ListOp::Remove(i) => Some((*i, 1, 0)),
            ListOp::Set(i, _) => Some((*i, 1, 1)),
            ListOp::Splice(s) => Some((s.at, s.remove, s.insert.len())),
            ListOp::Clear => None,
        }
    }

    /// The smallest op that replaces `remove` values at `at` with `insert`.
    pub(crate) fn splice(at: usize, remove: usize, mut insert: Vec<T>) -> Self {
        match (remove, insert.len()) {
            (0, 1) => ListOp::Insert(at, insert.pop().unwrap()),
            (1, 0) => ListOp::Remove(at),
            (1, 1) => ListOp::Set(at, insert.pop().unwrap()),
            _ => ListOp::Splice(Box::new(Splice { at, remove, insert })),
        }
    }

    pub(crate) fn apply_to(&self, items: &mut Vec<T>) -> Result<(), String>
    where
        T: Clone,
    {
        let len = items.len();
        let out_of_range = |i: usize| format!("index {i} is out of range for a list of length {len}");
        match self {
            ListOp::Insert(i, v) if *i <= len => items.insert(*i, v.clone()),
            ListOp::Remove(i) if *i < len => {
                items.remove(*i);
            }
            ListOp::Set(i, v) if *i < len => items[*i] = v.clone(),
            ListOp::Clear => items.clear(),
            ListOp::Splice(s) if s.at.checked_add(s.remove).is_some_and(|end| end <= len) => {
                items.splice(s.at..s.at + s.remove, s.insert.iter().cloned());
            }
            ListOp::Insert(i, _) | ListOp::Remove(i) | ListOp::Set(i, _) => return Err(out_of_range(*i)),
            ListOp::Splice(s) => return Err(out_of_range(s.at.saturating_add(s.remove))),
        }
        Ok(())
    }
}

/// Adds `op` to `ops`, merged into the last op if it continues right where
/// that one ended. Many pushes in a row then become a single `Splice`, and so
/// do the removes and inserts a diff produces for a replaced range.
pub(crate) fn push_op<T>(ops: &mut Vec<ListOp<T>>, op: ListOp<T>) {
    let Some(last) = ops.last_mut() else { return ops.push(op) };
    let (Some((start, _, inserted)), Some((at, _, _))) = (last.span(), op.span()) else { return ops.push(op) };
    if at != start + inserted {
        return ops.push(op);
    }
    if !matches!(last, ListOp::Splice(_)) {
        let (removed, values) = match std::mem::replace(last, ListOp::Clear) {
            ListOp::Insert(_, v) => (0, vec![v]),
            ListOp::Set(_, v) => (1, vec![v]),
            _ => (1, Vec::new()),
        };
        *last = ListOp::Splice(Box::new(Splice { at: start, remove: removed, insert: values }));
    }
    let ListOp::Splice(s) = last else { unreachable!() };
    match op {
        ListOp::Insert(_, v) => s.insert.push(v),
        ListOp::Remove(_) => s.remove += 1,
        ListOp::Set(_, v) => {
            s.remove += 1;
            s.insert.push(v);
        }
        ListOp::Splice(other) => {
            s.remove += other.remove;
            s.insert.extend(other.insert);
        }
        ListOp::Clear => unreachable!(),
    }
    // Remove + Insert at the same index is a Set.
    if s.remove == 1 && s.insert.len() == 1 {
        *last = ListOp::Set(start, s.insert.pop().unwrap());
    }
}

fn fmt_list_op<T: fmt::Debug>(name: &str, op: &ListOp<T>, f: &mut fmt::Formatter) -> fmt::Result {
    match op {
        ListOp::Insert(i, v) => write!(f, "{name}.insert({i}, {v:?})"),
        ListOp::Remove(i) => write!(f, "{name}.remove({i})"),
        ListOp::Set(i, v) => write!(f, "{name}[{i}] = {v:?}"),
        ListOp::Clear => write!(f, "{name}.clear()"),
        ListOp::Splice(s) => write!(f, "{name}.splice({}..{}, {:?})", s.at, s.at + s.remove, s.insert),
    }
}

/// Every `List` and `Map` gets a unique number when it's created. If a field
/// has a different number after an edit, the whole value was replaced (for
/// example with `g.members = List::new()` or `*g = Group { .. }`), and then
/// the new contents are logged in full.
pub(crate) fn new_origin() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A list that tracks its own changes and works like a `Vec`.
///
/// - Common changes (`push`, `insert`, `remove`, `set`, `swap`, `retain`,
///   `truncate`, `list[i] = x`, `get_mut` ...) are logged directly, at O(1)
///   extra cost per changed element regardless of the length of the list.
///   Adjacent changes made in the same edit are merged into one
///   [`ListOp::Splice`].
/// - The rest of the `Vec` API (`sort`, `iter_mut`, `drain`, `dedup` ...) is
///   reached through `DerefMut`. The list is then copied first, and the
///   changes are found with a diff when the edit ends. That costs O(n) for
///   the copy plus O(n·m) for the part of the list that changed (capped, see
///   below).
/// - If the whole list is replaced, that's noticed, and the new contents are
///   logged.
///
/// The diff skips the common start and end of the list, so a small change
/// to a long list stays cheap. If the changed middle part is still too large
/// to diff (more than about a million element pairs), it's logged as a
/// single splice that replaces the whole range.
///
/// ```
/// use replayable::List;
///
/// let mut tags = List::from(vec!["b".to_string()]);
/// tags.push("a".into());
/// tags.sort(); // the whole Vec API through DerefMut
/// assert_eq!(tags.as_slice(), ["a", "b"]);
/// ```
pub struct List<T> {
    items: Vec<T>,
    pending: Vec<ListOp<T>>,
    origin: u64,
    /// The element lent out through `list[i]` or `get_mut`, and its old value.
    touched: Option<(usize, T)>,
    /// The copy `deref_mut` took, to diff against.
    before: Option<Vec<T>>,
}

impl<T> List<T> {
    /// An empty list.
    pub fn new() -> Self {
        List::from(Vec::new())
    }

    /// The elements as a slice.
    pub fn as_slice(&self) -> &[T] {
        &self.items
    }
}

impl<T: Clone + PartialEq> List<T> {
    /// Turns elements lent out and changes made through `DerefMut` into ops.
    /// Runs first in every change, so the indices in new ops are always right.
    fn flush(&mut self) {
        if let Some((i, old)) = self.touched.take() {
            if self.items[i] != old {
                push_op(&mut self.pending, ListOp::Set(i, self.items[i].clone()));
            }
        }
        if let Some(before) = self.before.take() {
            list_diff(&before, &self.items, &mut self.pending);
        }
    }

    /// Appends a value, like `Vec::push`.
    pub fn push(&mut self, value: T) {
        self.flush();
        push_op(&mut self.pending, ListOp::Insert(self.items.len(), value.clone()));
        self.items.push(value);
    }

    /// Inserts a value at `index`, like `Vec::insert`.
    pub fn insert(&mut self, index: usize, value: T) {
        self.flush();
        self.items.insert(index, value.clone());
        push_op(&mut self.pending, ListOp::Insert(index, value));
    }

    /// Removes and returns the value at `index`, like `Vec::remove`.
    pub fn remove(&mut self, index: usize) -> T {
        self.flush();
        let value = self.items.remove(index);
        push_op(&mut self.pending, ListOp::Remove(index));
        value
    }

    /// Removes and returns the last value, like `Vec::pop`.
    pub fn pop(&mut self) -> Option<T> {
        self.flush();
        let value = self.items.pop()?;
        push_op(&mut self.pending, ListOp::Remove(self.items.len()));
        Some(value)
    }

    /// Replaces the value at `index` and returns the old one.
    pub fn set(&mut self, index: usize, value: T) -> T {
        self.flush();
        let old = std::mem::replace(&mut self.items[index], value.clone());
        push_op(&mut self.pending, ListOp::Set(index, value));
        old
    }

    /// Swaps two values, like `slice::swap`.
    pub fn swap(&mut self, a: usize, b: usize) {
        self.flush();
        self.items.swap(a, b);
        if a != b {
            push_op(&mut self.pending, ListOp::Set(a, self.items[a].clone()));
            push_op(&mut self.pending, ListOp::Set(b, self.items[b].clone()));
        }
    }

    /// Like `Vec::get_mut`. The change is logged as a `Set` if the value changed.
    pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        self.flush();
        let old = self.items.get(index)?.clone();
        self.touched = Some((index, old));
        self.items.get_mut(index)
    }

    /// Removes everything, like `Vec::clear`.
    pub fn clear(&mut self) {
        // Anything not flushed yet doesn't matter, it's all gone anyway.
        self.touched = None;
        self.before = None;
        self.items.clear();
        self.pending.push(ListOp::Clear);
    }

    /// Shortens the list to `len`, like `Vec::truncate`.
    pub fn truncate(&mut self, len: usize) {
        self.flush();
        if len < self.items.len() {
            let remove = self.items.len() - len;
            self.items.truncate(len);
            push_op(&mut self.pending, ListOp::splice(len, remove, Vec::new()));
        }
    }

    /// Like `Vec::retain`, O(n) no matter how many are removed.
    pub fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
        self.flush();
        let pending = &mut self.pending;
        let mut kept = 0;
        self.items.retain(|value| {
            let k = keep(value);
            if k {
                kept += 1;
            } else {
                push_op(pending, ListOp::Remove(kept));
            }
            k
        });
    }
}

impl<T: Clone + PartialEq> Extend<T> for List<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for value in iter {
            self.push(value);
        }
    }
}

/// `list[i]`, `list[a..b]` and the other slice indexes, like a `Vec`.
impl<T, I: SliceIndex<[T]>> Index<I> for List<T> {
    type Output = I::Output;
    fn index(&self, index: I) -> &I::Output {
        &self.items[index]
    }
}

/// `list[i] = x` and `list[i].foo()` are logged, just like `get_mut`.
impl<T: Clone + PartialEq> IndexMut<usize> for List<T> {
    fn index_mut(&mut self, index: usize) -> &mut T {
        let len = self.items.len();
        self.get_mut(index).unwrap_or_else(|| panic!("index out of bounds: the len is {len} but the index is {index}"))
    }
}

impl<T> Deref for List<T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Vec<T> {
        &self.items
    }
}

/// Gives the whole `Vec` API for what isn't tracked above. The list is
/// copied first, and on the next change (or when the edit ends) a diff is
/// computed.
impl<T: Clone + PartialEq> DerefMut for List<T> {
    fn deref_mut(&mut self) -> &mut Vec<T> {
        self.flush();
        self.before = Some(self.items.clone());
        &mut self.items
    }
}

impl<'a, T> IntoIterator for &'a List<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

impl<T> From<Vec<T>> for List<T> {
    fn from(items: Vec<T>) -> Self {
        List { items, pending: Vec::new(), origin: new_origin(), touched: None, before: None }
    }
}

impl<T> From<List<T>> for Vec<T> {
    fn from(list: List<T>) -> Self {
        list.items
    }
}

/// Written as a plain sequence, like a `Vec`.
impl<T: Serialize> Serialize for List<T> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.items.serialize(s)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for List<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Vec::deserialize(d).map(List::from)
    }
}

impl<T> FromIterator<T> for List<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        List::from(iter.into_iter().collect::<Vec<_>>())
    }
}

impl<T> Default for List<T> {
    fn default() -> Self {
        List::new()
    }
}

// A clone is a new list. Changes not yet logged belong to an edit in
// progress and don't come along.
impl<T: Clone> Clone for List<T> {
    fn clone(&self) -> Self {
        List::from(self.items.clone())
    }
}
impl<T: PartialEq> PartialEq for List<T> {
    fn eq(&self, other: &Self) -> bool {
        self.items == other.items
    }
}
impl<T: Eq> Eq for List<T> {}
impl<T: fmt::Debug> fmt::Debug for List<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.items.fmt(f)
    }
}

impl<T: Value> List<T> {
    /// Emits the whole contents, replacing what was there.
    fn log_in_full(&mut self, emit: &mut dyn FnMut(ListOp<T>)) {
        self.settle();
        emit(ListOp::Clear);
        if !self.items.is_empty() {
            emit(ListOp::splice(0, 0, self.items.clone()));
        }
    }
}

impl<T: Value> Field for List<T> {
    type Op = ListOp<T>;
    type Snapshot = u64;
    fn snapshot(&self) -> u64 {
        self.origin
    }
    fn changes(&mut self, origin: u64, emit: &mut dyn FnMut(ListOp<T>)) -> Result<(), String> {
        let replaced = self.origin != origin;
        if !replaced {
            self.flush();
        }
        let checked = if replaced { Self::check(self) } else { self.pending.iter().try_for_each(Self::check_op) };
        if let Err(reason) = checked {
            // Only values added in this edit can fail, since the others were
            // logged. Without them, the list is logged in full.
            self.items.retain(|value| Value::check(value).is_ok());
            self.log_in_full(emit);
            return Err(format!("{reason}; the elements that hold one were removed"));
        }
        if replaced {
            // A different list: log the new contents in full.
            self.log_in_full(emit);
        } else {
            std::mem::take(&mut self.pending).into_iter().for_each(emit);
        }
        Ok(())
    }
    fn settle(&mut self) {
        self.pending = Vec::new();
        self.touched = None;
        self.before = None;
    }
    fn apply(&mut self, op: &ListOp<T>) -> Result<(), String> {
        op.apply_to(&mut self.items)
    }
    fn check(&self) -> Result<(), String> {
        self.items.iter().try_for_each(T::check)
    }
    fn check_op(op: &ListOp<T>) -> Result<(), String> {
        match op {
            ListOp::Insert(_, v) | ListOp::Set(_, v) => v.check(),
            ListOp::Splice(s) => s.insert.iter().try_for_each(T::check),
            ListOp::Remove(_) | ListOp::Clear => Ok(()),
        }
    }
    fn fmt_op(name: &str, op: &ListOp<T>, f: &mut fmt::Formatter) -> fmt::Result {
        fmt_list_op(name, op, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ops<T>(list: &mut List<T>) -> Vec<ListOp<T>>
    where
        T: Clone + PartialEq,
    {
        list.flush();
        std::mem::take(&mut list.pending)
    }

    #[test]
    fn indexing_works_like_a_vec() {
        let mut list = List::from(vec![1, 2, 3, 4]);
        assert_eq!(list[1], 2);
        assert_eq!(&list[1..3], [2, 3]);
        assert_eq!(&list[2..], [3, 4]);
        list[0] = 9;
        assert_eq!(ops(&mut list), [ListOp::Set(0, 9)]);
    }

    #[test]
    fn adjacent_changes_are_merged() {
        let mut list = List::from(vec![1, 2, 3]);
        list.extend([4, 5, 6]);
        assert_eq!(ops(&mut list), [ListOp::splice(3, 0, vec![4, 5, 6])]);

        list.truncate(2);
        assert_eq!(ops(&mut list), [ListOp::splice(2, 4, vec![])]);

        list.pop();
        assert_eq!(ops(&mut list), [ListOp::Remove(1)]);

        let mut list = List::from(vec![1, 2, 3, 4, 5]);
        list.retain(|&x| x == 1 || x == 5);
        assert_eq!(ops(&mut list), [ListOp::splice(1, 3, vec![])]);

        // Changes far apart stay separate.
        let mut list = List::from(vec![1, 2, 3, 4]);
        list.set(0, 9);
        list.set(3, 9);
        assert_eq!(ops(&mut list), [ListOp::Set(0, 9), ListOp::Set(3, 9)]);
    }

    #[test]
    fn invalid_ops_are_rejected() {
        let mut items = vec![1, 2];
        assert!(ListOp::Insert(3, 0).apply_to(&mut items).is_err());
        assert!(ListOp::<i32>::Remove(2).apply_to(&mut items).is_err());
        assert!(ListOp::Set(2, 0).apply_to(&mut items).is_err());
        assert!(ListOp::<i32>::splice(1, 2, vec![]).apply_to(&mut items).is_err());
        assert!(ListOp::<i32>::splice(usize::MAX, 2, vec![]).apply_to(&mut items).is_err());
        assert_eq!(items, [1, 2]);
    }
}
