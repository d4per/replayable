//! The JSON shape of events, written by hand so that old logs can still be
//! read after fields or entity types are added, renamed or removed.
//!
//! `model!` generates the per-entity parts (which names exist, what they
//! decode to) as methods on [`Entity`] and [`Model`]. This module holds the
//! serde visitors that drive them.

use crate::{Entity, Model, Timestamp};
use serde::de::{self, Deserialize, Deserializer, IgnoredAny, MapAccess, Visitor};
use serde::ser::{Serialize, SerializeMap, Serializer};
use std::borrow::Cow;
use std::fmt;
use std::marker::PhantomData;

/// An event read from a log: either one to apply, or one that refers to a
/// removed entity type or field and is skipped. `id` is still needed for
/// skipped events, so that ids are never handed out twice.
#[doc(hidden)]
pub enum Parsed<E> {
    Event(E),
    Skipped { id: u64 },
}

/// Serializes `{key: value}`.
pub fn serialize_entry<S: Serializer, V: Serialize + ?Sized>(s: S, key: &str, value: &V) -> Result<S::Ok, S::Error> {
    let mut map = s.serialize_map(Some(1))?;
    map.serialize_entry(key, value)?;
    map.end()
}

/// The error for a name the model doesn't know.
pub fn unknown<E: de::Error>(what: &str, name: &str, hint: &str) -> E {
    E::custom(format_args!("unknown {what} `{name}`{hint}"))
}

/// The hint for an unknown field of `kind`.
pub fn unknown_field<E: de::Error>(kind: &str, name: &str) -> E {
    unknown(
        "field",
        name,
        &format!(
            " in {kind}. If it was removed, add `#![removed({name})]` to {kind}; \
             if it was renamed, write `new_name (was {name}): Type`"
        ),
    )
}

/// A map key, borrowed from the input when possible (serde_json does that
/// for keys without escapes), so reading a log doesn't allocate per key.
struct KeyStr<'de>(Cow<'de, str>);

impl<'de> Deserialize<'de> for KeyStr<'de> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct KeyVisitor;
        impl<'de> Visitor<'de> for KeyVisitor {
            type Value = KeyStr<'de>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a name")
            }
            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
                Ok(KeyStr(Cow::Borrowed(v)))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(KeyStr(Cow::Owned(v.to_owned())))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(KeyStr(Cow::Owned(v)))
            }
        }
        d.deserialize_str(KeyVisitor)
    }
}

fn one_key<'de, A: MapAccess<'de>>(map: &mut A, expecting: &dyn de::Expected) -> Result<KeyStr<'de>, A::Error> {
    map.next_key::<KeyStr>()?.ok_or_else(|| de::Error::invalid_length(0, expecting))
}

fn no_more_keys<'de, A: MapAccess<'de>>(map: &mut A, expecting: &dyn de::Expected) -> Result<(), A::Error> {
    match map.next_key::<IgnoredAny>()? {
        Some(_) => Err(de::Error::invalid_length(2, expecting)),
        None => Ok(()),
    }
}

/// Reads a `{field: op}` change to an `E`. `None` if the field was removed.
pub fn deserialize_change<'de, E: Entity, D: Deserializer<'de>>(d: D) -> Result<Option<E::Change>, D::Error> {
    struct ChangeVisitor<E>(PhantomData<E>);
    impl<'de, E: Entity> Visitor<'de> for ChangeVisitor<E> {
        type Value = Option<E::Change>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            write!(f, "a map with one field of {}", E::KIND)
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let field = one_key(&mut map, &self)?;
            let change = E::decode_change(&field.0, &mut map)?;
            no_more_keys(&mut map, &self)?;
            Ok(change)
        }
    }
    d.deserialize_map(ChangeVisitor::<E>(PhantomData))
}

/// A change that may be to a removed field, as it's read from a log.
pub struct MaybeChange<E: Entity>(pub Option<E::Change>);

impl<'de, E: Entity> Deserialize<'de> for MaybeChange<E> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        deserialize_change::<E, D>(d).map(MaybeChange)
    }
}

/// Writes an event as `{"<Kind><Entity>": ...}`. Used by the generated
/// `Serialize` for events.
pub fn serialize_event<M: Model, S: Serializer>(event: &M::Event, s: S) -> Result<S::Ok, S::Error> {
    let mut map = s.serialize_map(Some(1))?;
    M::serialize_event(event, &mut map)?;
    map.end()
}

/// The key of the time on each line of the log.
const AT: &str = "at";

/// Writes one line of the log, `{"at": time, "<Kind><Entity>": ...}`, without
/// the newline.
pub(crate) fn write_line<M: Model>(
    out: impl std::io::Write,
    at: Timestamp,
    event: &M::Event,
) -> serde_json::Result<()> {
    let mut s = serde_json::Serializer::new(out);
    let mut map = s.serialize_map(Some(2))?;
    map.serialize_entry(AT, &at)?;
    M::serialize_event(event, &mut map)?;
    map.end()
}

/// Writes an entity as `{field: value, ...}`.
pub struct FieldsOf<'a, E>(pub &'a E);

impl<E: Entity> Serialize for FieldsOf<'_, E> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize_fields(s)
    }
}

/// Reads an entity from `{field: value, ...}`. Fields that are missing keep
/// their default, which is how fields added to the model later get a value
/// for entities created before they existed.
pub struct EntityValue<E>(pub E);

impl<'de, E: Entity> Deserialize<'de> for EntityValue<E> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct EntityVisitor<E>(PhantomData<E>);
        impl<'de, E: Entity> Visitor<'de> for EntityVisitor<E> {
            type Value = E;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "the fields of a {}", E::KIND)
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<E, A::Error> {
                let mut entity = E::default();
                while let Some(field) = map.next_key::<KeyStr>()? {
                    entity.decode_field(&field.0, &mut map)?;
                }
                Ok(entity)
            }
        }
        d.deserialize_map(EntityVisitor(PhantomData)).map(EntityValue)
    }
}

/// Reads one event, `{"<Kind><Entity>": ...}`.
pub fn parse_event<'de, M: Model, D: Deserializer<'de>>(d: D) -> Result<Parsed<M::Event>, D::Error> {
    struct EventVisitor<M>(PhantomData<M>);
    impl<'de, M: Model> Visitor<'de> for EventVisitor<M> {
        type Value = Parsed<M::Event>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("an event: a map with one key, like `CreateUser`")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let tag = one_key(&mut map, &self)?;
            let event = M::decode_event(&tag.0, &mut map)?;
            no_more_keys(&mut map, &self)?;
            Ok(event)
        }
    }
    d.deserialize_map(EventVisitor::<M>(PhantomData))
}

/// Reads one line of the log: `{"at": time, "<Kind><Entity>": ...}`, with
/// the keys in any order.
pub(crate) fn parse_line<'de, M: Model, D: Deserializer<'de>>(d: D) -> Result<(Timestamp, Parsed<M::Event>), D::Error> {
    struct LineVisitor<M>(PhantomData<M>);
    impl<'de, M: Model> Visitor<'de> for LineVisitor<M> {
        type Value = (Timestamp, Parsed<M::Event>);
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("an event: a map with the time `at` and one more key, like `CreateUser`")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let (mut at, mut event) = (None, None);
            while let Some(key) = map.next_key::<KeyStr>()? {
                if key.0 == AT {
                    if at.is_some() {
                        return Err(de::Error::duplicate_field(AT));
                    }
                    at = Some(map.next_value()?);
                } else if event.is_some() {
                    return Err(de::Error::custom(format_args!("a second event `{}` on the same line", key.0)));
                } else {
                    event = Some(M::decode_event(&key.0, &mut map)?);
                }
            }
            let at = at.ok_or_else(|| de::Error::missing_field(AT))?;
            Ok((at, event.ok_or_else(|| de::Error::invalid_length(1, &self))?))
        }
    }
    d.deserialize_map(LineVisitor::<M>(PhantomData))
}

/// Reads one event, failing if it refers to something removed. Used by the
/// generated `Deserialize` for events.
pub fn deserialize_event<'de, M: Model, D: Deserializer<'de>>(d: D) -> Result<M::Event, D::Error> {
    match parse_event::<M, D>(d)? {
        Parsed::Event(event) => Ok(event),
        Parsed::Skipped { id } => {
            Err(de::Error::custom(format_args!("the event for id {id} is about a removed entity type or field")))
        }
    }
}

/// Reads one change, failing if it's to a removed field. Used by the
/// generated `Deserialize` for changes.
pub fn deserialize_present_change<'de, E: Entity, D: Deserializer<'de>>(d: D) -> Result<E::Change, D::Error> {
    deserialize_change::<E, D>(d)?
        .ok_or_else(|| de::Error::custom(format_args!("the change is to a removed field of {}", E::KIND)))
}

/// Panics with `message` about a value that can't be stored, unless the
/// thread is already panicking (a second panic would abort).
#[track_caller]
pub fn reject(message: std::fmt::Arguments) {
    if !std::thread::panicking() {
        panic!("{message}");
    }
}

/// In debug builds: checks that an event comes back the same after a trip
/// through JSON, so that a value type which can't be stored is found when
/// it's changed instead of when the log is loaded.
#[cfg(debug_assertions)]
#[track_caller]
pub fn assert_round_trip<M: Model>(event: &M::Event) {
    if std::thread::panicking() {
        return;
    }
    let json = match serde_json::to_string(event) {
        Ok(json) => json,
        Err(e) => panic!("the event `{event}` can't be written as JSON: {e}"),
    };
    let mut de = serde_json::Deserializer::from_str(&json);
    match parse_event::<M, _>(&mut de) {
        Ok(Parsed::Event(back)) if back == *event => {}
        Ok(Parsed::Event(back)) => panic!(
            "the event `{event}` changes on a trip through JSON, so a replay won't match. \
             Check the Serialize/Deserialize and PartialEq of its values.\n  logged: {event:?}\n  read back: {back:?}\n  json: {json}"
        ),
        Ok(Parsed::Skipped { .. }) => unreachable!("a live event is never about a removed name"),
        Err(e) => panic!(
            "the event `{event}` can't be read back from its JSON: {e}\n  json: {json}\n\
             (NaN and infinite floats are written as null, for example)"
        ),
    }
}
