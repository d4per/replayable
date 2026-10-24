#![doc = include_str!("../README.md")]
#![warn(missing_docs)]

mod binary;
mod codec;
mod diff;
mod error;
mod field;
mod id;
mod list;
mod macros;
mod map;
mod store;
mod time;

pub use error::Error;
pub use field::{Declared, Element, Field, Key, OpOf, Stored, Value, Weak};
pub use id::Id;
pub use list::{List, ListOp, Splice};
pub use map::{Map, MapOp};
pub use store::{Contains, Edit, Entity, Model, Store};
pub use time::{ParseTimestampError, Timestamp};

/// Used by `model!`. Not part of the public API.
#[doc(hidden)]
pub mod __private {
    pub use crate::codec::{
        deserialize_event, deserialize_present_change, serialize_entry, serialize_event, unknown, unknown_field,
        EntityValue, FieldsOf, MaybeChange, Parsed,
    };
    pub use crate::store::{clear_dead_weak, delete_dead};
    pub use paste;
    pub use serde;
    pub use std::collections::{btree_map, BTreeMap, BTreeSet};
}

/// Names used twice in `model!` are compile errors:
///
/// ```compile_fail,E0428
/// replayable::model! { W { U { #![removed(a)] a: u8 } } }
/// ```
///
/// ```compile_fail,E0428
/// replayable::model! { W { U { a: u8, b (was a): u8 } } }
/// ```
///
/// ```compile_fail,E0428
/// replayable::model! { W { U { a: u8 } V (was U) { b: u8 } } }
/// ```
///
/// ```compile_fail,E0428
/// replayable::model! { W { #![removed(U)] U { a: u8 } } }
/// ```
///
/// While all of this together is fine:
///
/// ```
/// replayable::model! { W { #![removed(T)] U { #![removed(x)] a: u8, b (was c, d): u8 = 2 } V (was X) {} } }
/// ```
#[cfg(doctest)]
pub struct CompileFailTests;
