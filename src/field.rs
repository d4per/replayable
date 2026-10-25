use crate::{Entity, Id, List, Map};
use serde::{de::DeserializeOwned, Serialize};
use std::fmt;
use std::marker::PhantomData;

/// A plain value that is stored as is and replaced as a whole when it changes.
///
/// Implemented for strings, numbers, `bool`, `char`, `Option<T>` and [`Id`].
/// Your own types (an enum, a small struct) opt in with an empty impl:
///
/// ```
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
/// enum Status {
///     #[default]
///     Active,
///     Banned { reason: String },
/// }
/// impl replayable::Value for Status {}
/// ```
///
/// `Default` isn't required by `Value` itself, but a type needs it to be used
/// directly as a field in [`model!`](crate::model). Inside a [`List`] or as
/// a [`Map`] key or value, it's not needed.
///
/// The `Serialize`/`Deserialize` impls must give back an equal value, or a
/// replay won't match. In debug builds every logged change is checked for
/// that, so `#[serde(skip)]` on a field or a lossy format is found when the
/// value is changed rather than when the log is loaded.
pub trait Value: Clone + PartialEq + fmt::Debug + Serialize + DeserializeOwned {
    /// Checks that the value can be stored, before it's logged. A value that
    /// fails makes the edit panic with the message, after undoing the change
    /// (see [`Store::edit`](crate::Store::edit)), so that the store still
    /// matches its log.
    ///
    /// Accepts everything by default. `f32` and `f64` reject NaN and
    /// infinity, which JSON can't represent (serde_json writes them as `null`,
    /// which can't be read back). `Option` rejects `Some(None)`, which JSON
    /// writes as `null`, the same as `None`. Override it in your own types if
    /// they hold floats or have other invariants:
    ///
    /// ```
    /// # use serde::{Deserialize, Serialize};
    /// #[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
    /// struct Point { x: f64, y: f64 }
    ///
    /// impl replayable::Value for Point {
    ///     fn check(&self) -> Result<(), String> {
    ///         self.x.check()?;
    ///         self.y.check()
    ///     }
    /// }
    /// # use replayable::Value;
    /// assert!(Point { x: f64::NAN, y: 0.0 }.check().is_err());
    /// ```
    fn check(&self) -> Result<(), String> {
        Ok(())
    }

    /// Visits the raw number of every [`Id`] in the value. The entities they
    /// point to are kept alive by [`Store::collect`](crate::Store::collect).
    ///
    /// Visits nothing by default. `Id` visits itself, and `Option` its
    /// content. Override it in your own types if they hold ids, or the
    /// entities they point to can be collected while still in use:
    ///
    /// ```
    /// # use serde::{Deserialize, Serialize};
    /// # use replayable::{model, Id, Value};
    /// # model! { World { User { name: String } } }
    /// #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    /// struct Vote { by: Id<User>, up: bool }
    ///
    /// impl Value for Vote {
    ///     fn refs(&self, visit: &mut dyn FnMut(u64)) {
    ///         self.by.refs(visit)
    ///     }
    /// }
    /// ```
    fn refs(&self, visit: &mut dyn FnMut(u64)) {
        let _ = visit;
    }

    /// Whether the value is written as JSON's `null`, like `None`. Used to
    /// reject `Some(None)`, which JSON can't tell apart from `None`.
    #[doc(hidden)]
    fn is_null(&self) -> bool {
        false
    }
}

macro_rules! impl_value {
    ($($t:ty),*) => { $( impl Value for $t {} )* };
}
impl_value!(String, bool, char, u8, u16, u32, u64, u128, i8, i16, i32, i64, i128, usize, isize);

macro_rules! impl_value_float {
    ($($t:ty),*) => { $(
        impl Value for $t {
            fn check(&self) -> Result<(), String> {
                if self.is_finite() {
                    Ok(())
                } else {
                    Err(format!("{self} can't be stored, since JSON has no way to write it"))
                }
            }
        }
    )* };
}
impl_value_float!(f32, f64);

impl<T: Value> Value for Option<T> {
    fn check(&self) -> Result<(), String> {
        match self {
            Some(value) if value.is_null() => {
                Err(format!("{self:?} can't be stored, since JSON writes it as null, the same as None"))
            }
            Some(value) => value.check(),
            None => Ok(()),
        }
    }
    fn is_null(&self) -> bool {
        self.is_none()
    }
    fn refs(&self, visit: &mut dyn FnMut(u64)) {
        if let Some(value) = self {
            value.refs(visit)
        }
    }
}
impl<E: Entity> Value for Id<E> {
    fn refs(&self, visit: &mut dyn FnMut(u64)) {
        visit(self.raw())
    }
}

/// A weak reference to an entity, used as a field type in
/// [`model!`](crate::model): `reply_to: Weak<Message>`, or
/// `seen_by: List<Weak<User>>`.
///
/// It's stored like a plain reference, as `Option<Id<E>>` (or an `Id<E>` in
/// a list or map), but it doesn't keep the entity alive. When
/// [`Store::collect`](crate::Store::collect) deletes the entity, a weak
/// field that points to it is set to `None`, and list elements and map
/// entries that hold it are removed. Those changes are logged like any
/// other.
///
/// Never constructed; it only names a field type.
pub struct Weak<E: ?Sized>(PhantomData<E>);

/// Maps the type you write in [`model!`](crate::model) to the type that is
/// stored:
///
/// | Declared                 | Stored                   |
/// |--------------------------|--------------------------|
/// | `name: String`           | `String`                 |
/// | `parent: User`           | `Option<Id<User>>`       |
/// | `tags: List<String>`     | `List<String>`           |
/// | `members: List<User>`    | `List<Id<User>>`         |
/// | `roles: Map<User, Role>` | `Map<Id<User>, Role>`    |
/// | `reply_to: Weak<Message>`| `Option<Id<Message>>`    |
/// | `seen_by: List<Weak<User>>` | `List<Id<User>>`      |
///
/// It also tells [`Store::collect`](crate::Store::collect) which entities a
/// field keeps alive: every reference, except a [`Weak`] one.
///
/// `Vec` and `BTreeMap` can't be used directly. Use [`List`] and [`Map`],
/// which have the same API but make sure every change ends up in the log.
pub trait Declared {
    /// The type of the field in the generated struct.
    type Stored: Field;
    /// Visits the raw number of every entity the field keeps alive.
    fn refs(stored: &Self::Stored, visit: &mut dyn FnMut(u64));
    /// Whether the field holds a weak reference to an entity that isn't
    /// alive.
    fn has_dead(stored: &Self::Stored, alive: &dyn Fn(u64) -> bool) -> bool {
        let _ = (stored, alive);
        false
    }
    /// Removes the weak references to entities that aren't alive.
    fn clear_dead(stored: &mut Self::Stored, alive: &dyn Fn(u64) -> bool) {
        let _ = (stored, alive);
    }
}

/// How a type is stored when it's an element of a [`List`] or a value in a
/// [`Map`]: values as themselves, entities as an [`Id`].
pub trait Element: Sized {
    /// The stored element type.
    type Elem: Value;
    /// Visits the raw number of every entity the element keeps alive.
    fn refs(elem: &Self::Elem, visit: &mut dyn FnMut(u64));
    /// Whether the element is a weak reference to an entity that isn't
    /// alive, so that it should be removed.
    fn dead(elem: &Self::Elem, alive: &dyn Fn(u64) -> bool) -> bool {
        let _ = (elem, alive);
        false
    }
}

/// How a type is stored when it's a key in a [`Map`]: like [`Element`], but
/// the stored type must be `Ord`.
pub trait Key: Sized {
    /// The stored key type.
    type Key: Value + Ord;
    /// Visits the raw number of every entity the key keeps alive.
    fn refs(key: &Self::Key, visit: &mut dyn FnMut(u64));
    /// Whether the key is a weak reference to an entity that isn't alive,
    /// so that its entry should be removed.
    fn dead(key: &Self::Key, alive: &dyn Fn(u64) -> bool) -> bool {
        let _ = (key, alive);
        false
    }
}

// The `?Sized` bounds on these impls stop the compiler from recursing forever
// when an entity refers to itself, like `parent: User` or
// `friends: List<User>`. Without them it tries to prove `User: Sized` while
// it's still working out what `User` looks like. The size is known anyway,
// since `Value`, `Element` and `Key` require `Sized`. For the same reason
// `Map` keys have their own trait, with `Ord` on the associated type, instead
// of a `where K::Elem: Ord` clause on the impl. `private_model_works` in
// tests/model.rs covers both.
#[allow(clippy::needless_maybe_sized)]
impl<T: Value + Default + ?Sized> Declared for T {
    type Stored = T;
    fn refs(stored: &T, visit: &mut dyn FnMut(u64)) {
        stored.refs(visit)
    }
}
#[allow(clippy::needless_maybe_sized)]
impl<T: Value + ?Sized> Element for T {
    type Elem = T;
    fn refs(elem: &T, visit: &mut dyn FnMut(u64)) {
        elem.refs(visit)
    }
}
#[allow(clippy::needless_maybe_sized)]
impl<T: Value + Ord + ?Sized> Key for T {
    type Key = T;
    fn refs(key: &T, visit: &mut dyn FnMut(u64)) {
        key.refs(visit)
    }
}
#[allow(clippy::needless_maybe_sized)]
impl<T: Element + ?Sized> Declared for List<T> {
    type Stored = List<T::Elem>;
    fn refs(list: &Self::Stored, visit: &mut dyn FnMut(u64)) {
        list.iter().for_each(|e| T::refs(e, visit))
    }
    fn has_dead(list: &Self::Stored, alive: &dyn Fn(u64) -> bool) -> bool {
        list.iter().any(|e| T::dead(e, alive))
    }
    fn clear_dead(list: &mut Self::Stored, alive: &dyn Fn(u64) -> bool) {
        list.retain(|e| !T::dead(e, alive))
    }
}
#[allow(clippy::needless_maybe_sized)]
impl<K: Key + ?Sized, V: Element + ?Sized> Declared for Map<K, V> {
    type Stored = Map<K::Key, V::Elem>;
    fn refs(map: &Self::Stored, visit: &mut dyn FnMut(u64)) {
        for (k, v) in map.iter() {
            K::refs(k, visit);
            V::refs(v, visit);
        }
    }
    fn has_dead(map: &Self::Stored, alive: &dyn Fn(u64) -> bool) -> bool {
        map.iter().any(|(k, v)| K::dead(k, alive) || V::dead(v, alive))
    }
    fn clear_dead(map: &mut Self::Stored, alive: &dyn Fn(u64) -> bool) {
        map.retain(|k, v| !K::dead(k, alive) && !V::dead(v, alive))
    }
}

// A weak reference is stored like a plain one, but visits nothing, and is
// the one thing that `clear_dead` removes.
#[allow(clippy::needless_maybe_sized)]
impl<E: Entity + ?Sized> Declared for Weak<E> {
    type Stored = Option<Id<E>>;
    fn refs(_: &Self::Stored, _: &mut dyn FnMut(u64)) {}
    fn has_dead(stored: &Self::Stored, alive: &dyn Fn(u64) -> bool) -> bool {
        stored.is_some_and(|id| !alive(id.raw()))
    }
    fn clear_dead(stored: &mut Self::Stored, alive: &dyn Fn(u64) -> bool) {
        if Self::has_dead(stored, alive) {
            *stored = None;
        }
    }
}
#[allow(clippy::needless_maybe_sized)]
impl<E: Entity + ?Sized> Element for Weak<E> {
    type Elem = Id<E>;
    fn refs(_: &Id<E>, _: &mut dyn FnMut(u64)) {}
    fn dead(id: &Id<E>, alive: &dyn Fn(u64) -> bool) -> bool {
        !alive(id.raw())
    }
}
#[allow(clippy::needless_maybe_sized)]
impl<E: Entity + ?Sized> Key for Weak<E> {
    type Key = Id<E>;
    fn refs(_: &Id<E>, _: &mut dyn FnMut(u64)) {}
    fn dead(id: &Id<E>, alive: &dyn Fn(u64) -> bool) -> bool {
        !alive(id.raw())
    }
}

/// The stored type for a declared field type. See [`Declared`].
pub type Stored<T> = <T as Declared>::Stored;
/// The change type logged for a declared field type.
pub type OpOf<T> = <Stored<T> as Field>::Op;

/// A stored field type. You never implement this yourself.
///
/// Changes are captured in two steps: [`snapshot`](Field::snapshot) before
/// the edit and [`changes`](Field::changes) after it. Types that track their
/// own changes ([`List`] and [`Map`]) use a number as snapshot and are never
/// copied.
pub trait Field: Default + PartialEq + fmt::Debug + Serialize + DeserializeOwned {
    /// One logged change to the field.
    type Op: Clone + PartialEq + fmt::Debug + Serialize + DeserializeOwned;
    /// What's needed to see what changed.
    type Snapshot;
    /// Taken before an edit.
    fn snapshot(&self) -> Self::Snapshot;
    /// Emits the changes made since `before` was taken.
    ///
    /// A change with a value that can't be stored (see [`Value::check`]) is
    /// undone instead of emitted, so that the field still matches what was
    /// logged, and the reason is returned. A plain value goes back to
    /// `before`; a [`List`] or [`Map`] loses the elements or entries that
    /// hold such a value, and is then logged in full.
    fn changes(&mut self, before: Self::Snapshot, emit: &mut dyn FnMut(Self::Op)) -> Result<(), String>;
    /// Forgets changes not yet emitted. Used by `create`, which logs the
    /// whole value instead.
    fn settle(&mut self);
    /// Applies a logged change during replay. Fails if it doesn't fit the
    /// current value, for example a list index out of range.
    fn apply(&mut self, op: &Self::Op) -> Result<(), String>;
    /// Checks every value in the field with [`Value::check`].
    fn check(&self) -> Result<(), String>;
    /// Checks every value in a change with [`Value::check`].
    fn check_op(op: &Self::Op) -> Result<(), String>;
    /// Writes a change in a human-readable form, used by `Display` on events.
    fn fmt_op(name: &str, op: &Self::Op, f: &mut fmt::Formatter) -> fmt::Result;
}

// Plain values: the snapshot is a copy, the change is the new value.
impl<T: Value + Default> Field for T {
    type Op = T;
    type Snapshot = T;
    fn snapshot(&self) -> T {
        self.clone()
    }
    fn changes(&mut self, before: T, emit: &mut dyn FnMut(T)) -> Result<(), String> {
        if *self == before {
            return Ok(());
        }
        if let Err(reason) = Value::check(self) {
            let reason = format!("{reason}; it was put back to {before:?}");
            *self = before;
            return Err(reason);
        }
        emit(self.clone());
        Ok(())
    }
    fn settle(&mut self) {}
    fn apply(&mut self, op: &T) -> Result<(), String> {
        *self = op.clone();
        Ok(())
    }
    fn check(&self) -> Result<(), String> {
        Value::check(self)
    }
    fn check_op(op: &T) -> Result<(), String> {
        Value::check(op)
    }
    fn fmt_op(name: &str, op: &T, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "set {name}={op:?}")
    }
}
