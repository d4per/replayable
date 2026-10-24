#![doc = include_str!("../README.md")]
#![warn(missing_docs)]

// Lets `model!`'s serde attribute, which has to name the crate as
// `::replayable`, work in this crate's own tests too.
extern crate self as replayable;

mod diff;
mod error;
mod field;
mod id;
mod list;
mod macros;
mod map;
mod store;

pub use error::Error;
pub use field::{Declared, Element, Field, Key, OpOf, Stored, Value};
pub use id::Id;
pub use list::{List, ListOp, Splice};
pub use map::{Map, MapOp};
pub use store::{Contains, Edit, Entity, Model, Store};

/// Used by `model!`. Not part of the public API.
#[doc(hidden)]
pub mod __private {
    pub use paste;
    pub use serde;
    pub use std::collections::{btree_map, BTreeMap};
}
