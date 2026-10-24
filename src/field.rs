use crate::{Entity, Id, List, Map};
use serde::{de::DeserializeOwned, Serialize};
use std::fmt;

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
    /// fails makes the edit panic with the message.
    ///
    /// Accepts everything by default. `f32` and `f64` reject NaN and
    /// infinity, which JSON can't represent (serde_json writes them as `null`,
    /// which can't be read back). Override it in your own types if they hold
    /// floats or have other invariants:
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
        self.as_ref().map_or(Ok(()), T::check)
    }
}
impl<E: Entity> Value for Id<E> {}

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
///
/// `Vec` and `BTreeMap` can't be used directly. Use [`List`] and [`Map`],
/// which have the same API but make sure every change ends up in the log.
pub trait Declared {
    /// The type of the field in the generated struct.
    type Stored: Field;
}

/// How a type is stored when it's an element of a [`List`] or a value in a
/// [`Map`]: values as themselves, entities as an [`Id`].
pub trait Element: Sized {
    /// The stored element type.
    type Elem: Value;
}

/// How a type is stored when it's a key in a [`Map`]: like [`Element`], but
/// the stored type must be `Ord`.
pub trait Key: Sized {
    /// The stored key type.
    type Key: Value + Ord;
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
}
#[allow(clippy::needless_maybe_sized)]
impl<T: Value + ?Sized> Element for T {
    type Elem = T;
}
#[allow(clippy::needless_maybe_sized)]
impl<T: Value + Ord + ?Sized> Key for T {
    type Key = T;
}
#[allow(clippy::needless_maybe_sized)]
impl<T: Element + ?Sized> Declared for List<T> {
    type Stored = List<T::Elem>;
}
#[allow(clippy::needless_maybe_sized)]
impl<K: Key + ?Sized, V: Element + ?Sized> Declared for Map<K, V> {
    type Stored = Map<K::Key, V::Elem>;
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
    fn changes(&mut self, before: Self::Snapshot, emit: &mut dyn FnMut(Self::Op));
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
    fn changes(&mut self, before: T, emit: &mut dyn FnMut(T)) {
        if *self != before {
            emit(self.clone());
        }
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
