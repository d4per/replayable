use crate::diff::map_diff;
use crate::field::{Field, Value};
use crate::list::new_origin;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::ops::{Deref, DerefMut};

/// A logged change to a [`Map`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MapOp<K, V> {
    /// Insert or replace a value.
    Insert(K, V),
    /// Remove a key.
    Remove(K),
    /// Remove everything.
    Clear,
}

impl<K: Ord + Clone + fmt::Debug, V: Clone> MapOp<K, V> {
    pub(crate) fn apply_to(&self, items: &mut BTreeMap<K, V>) -> Result<(), String> {
        match self {
            MapOp::Insert(k, v) => {
                items.insert(k.clone(), v.clone());
            }
            MapOp::Remove(k) => {
                items.remove(k).ok_or_else(|| format!("key {k:?} isn't in the map"))?;
            }
            MapOp::Clear => items.clear(),
        }
        Ok(())
    }
}

/// A map (sorted by key) that tracks its own changes and works like a
/// `BTreeMap`, the same way [`List`](crate::List) works like a `Vec`:
///
/// - `insert`, `remove`, `retain`, `get_mut` ... are logged directly.
/// - The rest of the API (for example `entry`) is reached through
///   `DerefMut`, which copies the map and diffs it when the edit ends.
/// - If the whole map is replaced, the new contents are logged.
///
/// ```
/// use replayable::Map;
///
/// let mut scores = Map::new();
/// scores.insert("a", 1);
/// *scores.entry("a").or_default() += 1; // through DerefMut
/// assert_eq!(scores[&"a"], 2);
/// ```
pub struct Map<K, V> {
    items: BTreeMap<K, V>,
    pending: Vec<MapOp<K, V>>,
    origin: u64,
    /// The value lent out through `get_mut`, with its key and old value.
    touched: Option<(K, V)>,
    /// The copy `deref_mut` took, to diff against.
    before: Option<BTreeMap<K, V>>,
}

impl<K, V> Map<K, V> {
    /// An empty map.
    pub fn new() -> Self {
        Map::from(BTreeMap::new())
    }
}

impl<K: Ord + Clone, V: Clone + PartialEq> Map<K, V> {
    /// Like `List::flush`.
    fn flush(&mut self) {
        if let Some((k, old)) = self.touched.take() {
            let new = &self.items[&k];
            if *new != old {
                self.pending.push(MapOp::Insert(k, new.clone()));
            }
        }
        if let Some(before) = self.before.take() {
            map_diff(&before, &self.items, &mut self.pending);
        }
    }

    /// Inserts a value and returns the old one, like `BTreeMap::insert`.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.flush();
        self.pending.push(MapOp::Insert(key.clone(), value.clone()));
        self.items.insert(key, value)
    }

    /// Removes a key and returns its value, like `BTreeMap::remove`.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        self.flush();
        let value = self.items.remove(key)?;
        self.pending.push(MapOp::Remove(key.clone()));
        Some(value)
    }

    /// Like `BTreeMap::get_mut`. The change is logged as an `Insert` if the
    /// value changed.
    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.flush();
        let (k, v) = self.items.get_key_value(key)?;
        self.touched = Some((k.clone(), v.clone()));
        self.items.get_mut(key)
    }

    /// Removes everything, like `BTreeMap::clear`.
    pub fn clear(&mut self) {
        self.touched = None;
        self.before = None;
        self.items.clear();
        self.pending.push(MapOp::Clear);
    }

    /// Like `BTreeMap::retain`, but the closure only gets to read the values.
    pub fn retain(&mut self, mut keep: impl FnMut(&K, &V) -> bool) {
        self.flush();
        let pending = &mut self.pending;
        self.items.retain(|k, v| {
            let kept = keep(k, v);
            if !kept {
                pending.push(MapOp::Remove(k.clone()));
            }
            kept
        });
    }
}

impl<K: Ord + Clone, V: Clone + PartialEq> Extend<(K, V)> for Map<K, V> {
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        for (k, v) in iter {
            self.insert(k, v);
        }
    }
}

impl<K, V> Deref for Map<K, V> {
    type Target = BTreeMap<K, V>;
    fn deref(&self) -> &BTreeMap<K, V> {
        &self.items
    }
}

/// Gives the whole `BTreeMap` API, the same way `DerefMut` does for `List`.
impl<K: Ord + Clone, V: Clone + PartialEq> DerefMut for Map<K, V> {
    fn deref_mut(&mut self) -> &mut BTreeMap<K, V> {
        self.flush();
        self.before = Some(self.items.clone());
        &mut self.items
    }
}

impl<'a, K, V> IntoIterator for &'a Map<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = std::collections::btree_map::Iter<'a, K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

impl<K, V> From<BTreeMap<K, V>> for Map<K, V> {
    fn from(items: BTreeMap<K, V>) -> Self {
        Map { items, pending: Vec::new(), origin: new_origin(), touched: None, before: None }
    }
}

impl<K, V> From<Map<K, V>> for BTreeMap<K, V> {
    fn from(map: Map<K, V>) -> Self {
        map.items
    }
}

/// Written as a sequence of `[key, value]` pairs, so that any key type works
/// in JSON (where object keys must be strings).
impl<K: Serialize, V: Serialize> Serialize for Map<K, V> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(&self.items)
    }
}

impl<'de, K: Deserialize<'de> + Ord, V: Deserialize<'de>> Deserialize<'de> for Map<K, V> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Vec::<(K, V)>::deserialize(d).map(|pairs| pairs.into_iter().collect())
    }
}

impl<K: Ord, V> FromIterator<(K, V)> for Map<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Map::from(iter.into_iter().collect::<BTreeMap<_, _>>())
    }
}

impl<K, V> Default for Map<K, V> {
    fn default() -> Self {
        Map::new()
    }
}

impl<K: Clone, V: Clone> Clone for Map<K, V> {
    fn clone(&self) -> Self {
        Map::from(self.items.clone())
    }
}
impl<K: PartialEq, V: PartialEq> PartialEq for Map<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.items == other.items
    }
}
impl<K: Eq, V: Eq> Eq for Map<K, V> {}
impl<K: fmt::Debug, V: fmt::Debug> fmt::Debug for Map<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.items.fmt(f)
    }
}

impl<K: Value + Ord, V: Value> Field for Map<K, V> {
    type Op = MapOp<K, V>;
    type Snapshot = u64;
    fn snapshot(&self) -> u64 {
        self.origin
    }
    fn changes(&mut self, origin: u64, emit: &mut dyn FnMut(MapOp<K, V>)) {
        if self.origin != origin {
            // A different map: log the new contents in full.
            self.settle();
            emit(MapOp::Clear);
            self.items.iter().for_each(|(k, v)| emit(MapOp::Insert(k.clone(), v.clone())));
        } else {
            self.flush();
            std::mem::take(&mut self.pending).into_iter().for_each(emit);
        }
    }
    fn settle(&mut self) {
        self.pending = Vec::new();
        self.touched = None;
        self.before = None;
    }
    fn apply(&mut self, op: &MapOp<K, V>) -> Result<(), String> {
        op.apply_to(&mut self.items)
    }
    fn check(&self) -> Result<(), String> {
        self.items.iter().try_for_each(|(k, v)| k.check().and_then(|()| v.check()))
    }
    fn check_op(op: &MapOp<K, V>) -> Result<(), String> {
        match op {
            MapOp::Insert(k, v) => k.check().and_then(|()| v.check()),
            MapOp::Remove(k) => k.check(),
            MapOp::Clear => Ok(()),
        }
    }
    fn fmt_op(name: &str, op: &MapOp<K, V>, f: &mut fmt::Formatter) -> fmt::Result {
        match op {
            MapOp::Insert(k, v) => write!(f, "{name}.insert({k:?}, {v:?})"),
            MapOp::Remove(k) => write!(f, "{name}.remove({k:?})"),
            MapOp::Clear => write!(f, "{name}.clear()"),
        }
    }
}
