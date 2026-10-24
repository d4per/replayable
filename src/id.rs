use crate::Entity;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::marker::PhantomData;

/// A typed reference to an entity of type `T`.
///
/// Ids are handed out by [`Store::create`](crate::Store::create) and are
/// unique across all entity types in a store. An `Id<User>` can't be used
/// where an `Id<Group>` is expected. Serialized as a plain number.
pub struct Id<T> {
    raw: u64,
    _type: PhantomData<fn() -> T>,
}

impl<T> Id<T> {
    /// Rebuilds an id from its raw number, for example one read from a URL.
    ///
    /// Nothing checks that an entity with this id exists or that it has type
    /// `T`. Use [`Store::get`](crate::Store::get) or
    /// [`Store::contains`](crate::Store::contains) to find out.
    pub fn from_raw(raw: u64) -> Self {
        Id { raw, _type: PhantomData }
    }

    /// The raw number behind the id.
    pub fn raw(&self) -> u64 {
        self.raw
    }
}

// Manual impls so that T doesn't need the corresponding traits.
impl<T> Clone for Id<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Id<T> {}
impl<T> PartialEq for Id<T> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}
impl<T> Eq for Id<T> {}
impl<T> PartialOrd for Id<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<T> Ord for Id<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.raw.cmp(&other.raw)
    }
}
impl<T> std::hash::Hash for Id<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.raw.hash(state)
    }
}
impl<T: Entity> fmt::Debug for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} id:{}", T::KIND, self.raw)
    }
}
impl<T> Serialize for Id<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.raw.serialize(s)
    }
}
impl<'de, T> Deserialize<'de> for Id<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        u64::deserialize(d).map(Id::from_raw)
    }
}
