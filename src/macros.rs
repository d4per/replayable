/// Declares a model: a set of entity types and the event type that records
/// every change to them.
///
/// ```
/// use replayable::{model, List, Map, Store};
///
/// model! {
///     /// Everything in the app.
///     pub World {
///         /// A person.
///         User {
///             name: String,
///             age: u8,
///             parent: User,          // a reference to another User
///             tags: List<String>,    // the whole Vec API, every change logged
///         }
///         Group {
///             title: String,
///             members: List<User>,       // a list of references
///             roles: Map<User, String>,  // a map with references as keys
///         }
///     }
/// }
///
/// let mut store = Store::<World>::new();
/// let alice = store.create(User { name: "Alice".into(), ..Default::default() });
/// store.edit(alice).tags.push("admin".into());
/// assert_eq!(store[alice].tags.as_slice(), ["admin"]);
/// ```
///
/// For a model `World` with entities `User` and `Group`, this generates:
///
/// - `pub struct User { pub name: String, pub age: u8, pub parent: Option<Id<User>>, pub tags: List<String> }`,
///   with `Default` (see below), `Debug`, `Clone` and `PartialEq`. A field
///   declared as another entity type is stored as an optional
///   [`Id`](crate::Id), and [`List`](crate::List)/[`Map`](crate::Map) of
///   entities hold ids. See [`Declared`](crate::Declared) for the full
///   mapping.
/// - `enum UserChange { Name(String), Age(u8), Parent(Option<Id<User>>), Tags(ListOp<String>) }`:
///   one variant per field, in `CamelCase`.
/// - `enum WorldEvent { CreateUser(Id<User>, Box<User>), UpdateUser(Id<User>, UserChange), DeleteUser(Id<User>), PinUser(Id<User>), UnpinUser(Id<User>), CreateGroup(..), .. }`,
///   which is what [`Store::events`](crate::Store::events) holds and what
///   is written as JSON.
/// - `struct World`, the state, to use as `Store<World>`.
///
/// The visibility before the model name (`pub` above) applies to every
/// generated item and field. Leave it out to keep them private to the
/// module. Doc comments and other attributes can be put on the model, on
/// entities and on fields.
///
/// Field types must be [`Value`](crate::Value)s that implement `Default`,
/// entity types from the same model, [`Weak`](crate::Weak) references to
/// them, or `List`/`Map` of those.
///
/// # Changing the model
///
/// Logs written by an older version of the model can still be read after
/// these changes:
///
/// ```
/// use replayable::model;
///
/// model! {
///     pub World {
///         #![removed(Team)]                        // entity types that no longer exist
///         User {
///             #![removed(age, status)]             // fields that no longer exist
///             name: String,
///             level: u8 = 1,                       // a new field, and its value for old users
///             nickname (was nick): Option<String>, // a renamed field
///         }
///         Person (was Human) { title: String }     // a renamed entity type
///     }
/// }
///
/// assert_eq!(User::default().level, 1);
/// ```
///
/// - **Adding a field**: entities created before it existed get its default:
///   the `= value` after the type, or the type's `Default`. Entities created
///   later keep the value they were created with, even if the default is
///   changed again, since `create` logs every field.
/// - **Removing a field or entity type**: list it in `#![removed(..)]` at the
///   top of the entity (for fields) or of the model (for entity types). Old
///   events about it are then skipped. A name the model doesn't know and
///   that isn't listed as removed is a load error, so a typo or a forgotten
///   rename can't silently drop data.
/// - **Renaming**: `new_name (was old_name)`, for fields and entity types.
///   Several old names can be listed: `(was a, b)`.
///
/// Using the same name twice, for example as a field and as removed, is a
/// compile error.
///
/// The `= value` default is evaluated every time an entity is replayed or
/// created with `..Default::default()`, so it should always give the same
/// value: a literal or a constant, not the current time.
#[macro_export]
macro_rules! model {
    (@default) => { ::core::default::Default::default() };
    (@default $default:expr) => { $default };

    (
        $(#[$model_attr:meta])*
        $vis:vis $model:ident {
            $( #![removed($($gone_entity:ident),* $(,)?)] )?
            $(
                $(#[$entity_attr:meta])*
                $entity:ident $( (was $($old_entity:ident),+ $(,)?) )? {
                    $( #![removed($($gone_field:ident),* $(,)?)] )?
                    $(
                        $(#[$field_attr:meta])*
                        $field:ident $( (was $($old_field:ident),+ $(,)?) )? : $ty:ty $(= $default:expr)?
                    ),* $(,)?
                }
            )*
        }
    ) => { $crate::__private::paste::paste! {

        $(
            $(#[$entity_attr])*
            #[derive(::core::fmt::Debug, ::core::clone::Clone, ::core::cmp::PartialEq)]
            $vis struct $entity {
                $( $(#[$field_attr])* $vis $field: $crate::Stored<$ty>, )*
            }

            // Every name the entity has had, as variants: one used twice (as a
            // field and as removed, say) is then a compile error.
            const _: () = {
                #[allow(non_camel_case_types, dead_code)]
                enum FieldNames { $( $field, $($($old_field,)+)? )* $($($gone_field,)*)? }
            };

            impl ::core::default::Default for $entity {
                fn default() -> Self {
                    $entity { $( $field: $crate::model!(@default $($default)?), )* }
                }
            }

            // When the entity is used as a field type, a reference is stored.
            impl $crate::Declared for $entity {
                type Stored = ::core::option::Option<$crate::Id<$entity>>;
                fn refs(stored: &Self::Stored, visit: &mut dyn ::core::ops::FnMut(u64)) {
                    if let ::core::option::Option::Some(id) = stored {
                        visit(id.raw())
                    }
                }
            }
            impl $crate::Element for $entity {
                type Elem = $crate::Id<$entity>;
                fn refs(id: &Self::Elem, visit: &mut dyn ::core::ops::FnMut(u64)) {
                    visit(id.raw())
                }
            }
            impl $crate::Key for $entity {
                type Key = $crate::Id<$entity>;
                fn refs(id: &Self::Key, visit: &mut dyn ::core::ops::FnMut(u64)) {
                    visit(id.raw())
                }
            }

            #[doc = ::core::concat!("A logged change to one field of [`", ::core::stringify!($entity), "`].")]
            #[derive(::core::fmt::Debug, ::core::clone::Clone, ::core::cmp::PartialEq)]
            $vis enum [<$entity Change>] {
                $(
                    #[doc = ::core::concat!("A change to `", ::core::stringify!($field), "`.")]
                    [<$field:camel>]($crate::OpOf<$ty>),
                )*
            }

            impl ::core::fmt::Display for [<$entity Change>] {
                fn fmt(&self, f: &mut ::core::fmt::Formatter) -> ::core::fmt::Result {
                    match self {
                        $( Self::[<$field:camel>](op) =>
                            <$crate::Stored<$ty> as $crate::Field>::fmt_op(::core::stringify!($field), op, f), )*
                        #[allow(unreachable_patterns)]
                        _ => ::core::unreachable!(),
                    }
                }
            }

            // Written as `{"field": op}`.
            impl $crate::__private::serde::Serialize for [<$entity Change>] {
                fn serialize<S: $crate::__private::serde::Serializer>(&self, s: S) -> ::core::result::Result<S::Ok, S::Error> {
                    match self {
                        $( Self::[<$field:camel>](op) =>
                            $crate::__private::serialize_entry(s, ::core::stringify!($field), op), )*
                        #[allow(unreachable_patterns)]
                        _ => ::core::unreachable!(),
                    }
                }
            }

            impl<'de> $crate::__private::serde::Deserialize<'de> for [<$entity Change>] {
                fn deserialize<D: $crate::__private::serde::Deserializer<'de>>(d: D) -> ::core::result::Result<Self, D::Error> {
                    $crate::__private::deserialize_present_change::<$entity, D>(d)
                }
            }

            // What's needed to see what changed. For List and Map fields it's
            // just a number, so no copy at all.
            #[doc(hidden)]
            $vis struct [<$entity Snapshot>] {
                $( $field: <$crate::Stored<$ty> as $crate::Field>::Snapshot, )*
            }

            impl $crate::Entity for $entity {
                const KIND: &'static str = ::core::stringify!($entity);
                type Change = [<$entity Change>];
                type Snapshot = [<$entity Snapshot>];

                fn snapshot(&self) -> Self::Snapshot {
                    [<$entity Snapshot>] {
                        $( $field: <$crate::Stored<$ty> as $crate::Field>::snapshot(&self.$field), )*
                    }
                }

                fn changes(&mut self, before: Self::Snapshot, emit: &mut dyn ::core::ops::FnMut(Self::Change)) {
                    $(
                        <$crate::Stored<$ty> as $crate::Field>::changes(
                            &mut self.$field,
                            before.$field,
                            &mut |op| emit([<$entity Change>]::[<$field:camel>](op)),
                        );
                    )*
                }

                fn settle(&mut self) {
                    $( <$crate::Stored<$ty> as $crate::Field>::settle(&mut self.$field); )*
                }

                fn apply(&mut self, change: &Self::Change) -> ::core::result::Result<(), ::std::string::String> {
                    match change {
                        $( [<$entity Change>]::[<$field:camel>](op) =>
                            <$crate::Stored<$ty> as $crate::Field>::apply(&mut self.$field, op)
                                .map_err(|e| ::std::format!("{}: {e}", ::core::stringify!($field))), )*
                        #[allow(unreachable_patterns)]
                        _ => ::core::unreachable!(),
                    }
                }

                fn check(&self) -> ::core::result::Result<(), ::std::string::String> {
                    $(
                        <$crate::Stored<$ty> as $crate::Field>::check(&self.$field)
                            .map_err(|e| ::std::format!("{}: {e}", ::core::stringify!($field)))?;
                    )*
                    ::core::result::Result::Ok(())
                }

                fn check_change(change: &Self::Change) -> ::core::result::Result<(), ::std::string::String> {
                    match change {
                        $( [<$entity Change>]::[<$field:camel>](op) =>
                            <$crate::Stored<$ty> as $crate::Field>::check_op(op)
                                .map_err(|e| ::std::format!("{}: {e}", ::core::stringify!($field))), )*
                        #[allow(unreachable_patterns)]
                        _ => ::core::unreachable!(),
                    }
                }

                #[allow(unused_variables)]
                fn refs(&self, visit: &mut dyn ::core::ops::FnMut(u64)) {
                    $( <$ty as $crate::Declared>::refs(&self.$field, visit); )*
                }

                #[allow(unused_variables)]
                fn has_dead_weak(&self, alive: &dyn ::core::ops::Fn(u64) -> bool) -> bool {
                    false $( || <$ty as $crate::Declared>::has_dead(&self.$field, alive) )*
                }

                #[allow(unused_variables)]
                fn clear_dead_weak(&mut self, alive: &dyn ::core::ops::Fn(u64) -> bool) {
                    $( <$ty as $crate::Declared>::clear_dead(&mut self.$field, alive); )*
                }

                fn serialize_fields<S: $crate::__private::serde::Serializer>(&self, s: S) -> ::core::result::Result<S::Ok, S::Error> {
                    use $crate::__private::serde::ser::SerializeMap;
                    let mut map = s.serialize_map(::core::option::Option::None)?;
                    $( map.serialize_entry(::core::stringify!($field), &self.$field)?; )*
                    map.end()
                }

                fn decode_field<'de, A: $crate::__private::serde::de::MapAccess<'de>>(
                    &mut self,
                    field: &str,
                    map: &mut A,
                ) -> ::core::result::Result<(), A::Error> {
                    match field {
                        $( ::core::stringify!($field) $($( | ::core::stringify!($old_field) )+)? => {
                            self.$field = map.next_value()?;
                            ::core::result::Result::Ok(())
                        } )*
                        $($( ::core::stringify!($gone_field) => {
                            map.next_value::<$crate::__private::serde::de::IgnoredAny>()?;
                            ::core::result::Result::Ok(())
                        } )*)?
                        other => ::core::result::Result::Err(
                            $crate::__private::unknown_field(::core::stringify!($entity), other)),
                    }
                }

                fn decode_change<'de, A: $crate::__private::serde::de::MapAccess<'de>>(
                    field: &str,
                    map: &mut A,
                ) -> ::core::result::Result<::core::option::Option<Self::Change>, A::Error> {
                    match field {
                        $( ::core::stringify!($field) $($( | ::core::stringify!($old_field) )+)? =>
                            ::core::result::Result::Ok(::core::option::Option::Some(
                                [<$entity Change>]::[<$field:camel>](map.next_value()?))), )*
                        $($( ::core::stringify!($gone_field) => {
                            map.next_value::<$crate::__private::serde::de::IgnoredAny>()?;
                            ::core::result::Result::Ok(::core::option::Option::None)
                        } )*)?
                        other => ::core::result::Result::Err(
                            $crate::__private::unknown_field(::core::stringify!($entity), other)),
                    }
                }
            }
        )*

        #[doc = ::core::concat!("An event in the log of [`", ::core::stringify!($model), "`].")]
        #[derive(::core::fmt::Debug, ::core::clone::Clone, ::core::cmp::PartialEq)]
        $vis enum [<$model Event>] {
            $(
                #[doc = ::core::concat!("A [`", ::core::stringify!($entity), "`] was created with this value.")]
                [<Create $entity>]($crate::Id<$entity>, ::std::boxed::Box<$entity>),
                #[doc = ::core::concat!("A field of a [`", ::core::stringify!($entity), "`] changed.")]
                [<Update $entity>]($crate::Id<$entity>, [<$entity Change>]),
                #[doc = ::core::concat!("A [`", ::core::stringify!($entity), "`] was deleted.")]
                [<Delete $entity>]($crate::Id<$entity>),
                #[doc = ::core::concat!("A [`", ::core::stringify!($entity), "`] was pinned, so it's kept alive.")]
                [<Pin $entity>]($crate::Id<$entity>),
                #[doc = ::core::concat!("A [`", ::core::stringify!($entity), "`] was unpinned.")]
                [<Unpin $entity>]($crate::Id<$entity>),
            )*
        }

        impl ::core::fmt::Display for [<$model Event>] {
            #[allow(unused_assignments, unused_mut, unused_variables)]
            fn fmt(&self, f: &mut ::core::fmt::Formatter) -> ::core::fmt::Result {
                match self {
                    $(
                        Self::[<Create $entity>](id, entity) => {
                            ::core::write!(f, "create {id:?} {{")?;
                            let mut sep = " ";
                            $(
                                ::core::write!(f, "{sep}{}: {:?}", ::core::stringify!($field), entity.$field)?;
                                sep = ", ";
                            )*
                            ::core::write!(f, " }}")
                        }
                        Self::[<Update $entity>](id, c) => ::core::write!(f, "{id:?} {c}"),
                        Self::[<Delete $entity>](id) => ::core::write!(f, "delete {id:?}"),
                        Self::[<Pin $entity>](id) => ::core::write!(f, "pin {id:?}"),
                        Self::[<Unpin $entity>](id) => ::core::write!(f, "unpin {id:?}"),
                    )*
                }
            }
        }

        impl $crate::__private::serde::Serialize for [<$model Event>] {
            fn serialize<S: $crate::__private::serde::Serializer>(&self, s: S) -> ::core::result::Result<S::Ok, S::Error> {
                $crate::__private::serialize_event::<$model, S>(self, s)
            }
        }

        impl<'de> $crate::__private::serde::Deserialize<'de> for [<$model Event>] {
            fn deserialize<D: $crate::__private::serde::Deserializer<'de>>(d: D) -> ::core::result::Result<Self, D::Error> {
                $crate::__private::deserialize_event::<$model, D>(d)
            }
        }

        // The same for entity type names.
        const _: () = {
            #[allow(dead_code)]
            enum EntityNames { $( $entity, $($($old_entity,)+)? )* $($($gone_entity,)*)? }
        };

        $(#[$model_attr])*
        #[derive(::core::default::Default)]
        #[allow(non_snake_case)]
        $vis struct $model {
            $( $entity: $crate::__private::BTreeMap<$crate::Id<$entity>, $entity>, )*
            __pinned: $crate::__private::BTreeSet<u64>,
        }

        impl $crate::Model for $model {
            type Event = [<$model Event>];

            fn apply(&mut self, event: &Self::Event) -> ::core::result::Result<(), ::std::string::String> {
                match event {
                    $(
                        [<$model Event>]::[<Create $entity>](id, entity) => match self.$entity.entry(*id) {
                            $crate::__private::btree_map::Entry::Vacant(slot) => {
                                slot.insert(::core::clone::Clone::clone(&**entity));
                            }
                            $crate::__private::btree_map::Entry::Occupied(_) =>
                                return ::core::result::Result::Err(::std::format!("{id:?} already exists")),
                        },
                        [<$model Event>]::[<Update $entity>](id, change) => match self.$entity.get_mut(id) {
                            ::core::option::Option::Some(e) => $crate::Entity::apply(e, change)
                                .map_err(|e| ::std::format!("{id:?} {e}"))?,
                            ::core::option::Option::None =>
                                return ::core::result::Result::Err(::std::format!("{id:?} doesn't exist")),
                        },
                        [<$model Event>]::[<Delete $entity>](id) => {
                            if self.$entity.remove(id).is_none() {
                                return ::core::result::Result::Err(::std::format!("{id:?} doesn't exist"));
                            }
                            // Logs from before pins existed delete explicitly.
                            self.__pinned.remove(&id.raw());
                        }
                        [<$model Event>]::[<Pin $entity>](id) => {
                            if !self.$entity.contains_key(id) {
                                return ::core::result::Result::Err(::std::format!("{id:?} doesn't exist"));
                            }
                            if !self.__pinned.insert(id.raw()) {
                                return ::core::result::Result::Err(::std::format!("{id:?} is already pinned"));
                            }
                        }
                        [<$model Event>]::[<Unpin $entity>](id) => {
                            if !self.$entity.contains_key(id) || !self.__pinned.remove(&id.raw()) {
                                return ::core::result::Result::Err(::std::format!("{id:?} isn't pinned"));
                            }
                        }
                    )*
                }
                ::core::result::Result::Ok(())
            }

            fn pins(&self) -> &$crate::__private::BTreeSet<u64> {
                &self.__pinned
            }

            fn pins_mut(&mut self) -> &mut $crate::__private::BTreeSet<u64> {
                &mut self.__pinned
            }

            fn is_empty(&self) -> bool {
                true $( && self.$entity.is_empty() )*
            }

            #[allow(unused_variables)]
            fn refs_of(&self, raw: u64, visit: &mut dyn ::core::ops::FnMut(u64)) -> bool {
                $(
                    if let ::core::option::Option::Some(e) = self.$entity.get(&$crate::Id::from_raw(raw)) {
                        $crate::Entity::refs(e, visit);
                        return true;
                    }
                )*
                false
            }

            #[allow(unused_variables)]
            fn sweep(store: &mut $crate::Store<Self>, alive: &dyn ::core::ops::Fn(u64) -> bool) -> usize {
                // Weak references to the dead first, so that every change is
                // logged before the deletes.
                $( $crate::__private::clear_dead_weak::<$model, $entity>(store, alive); )*
                0 $( + $crate::__private::delete_dead::<$model, $entity>(store, alive) )*
            }

            fn entity_id(event: &Self::Event) -> u64 {
                match event {
                    $(
                        [<$model Event>]::[<Create $entity>](id, _)
                        | [<$model Event>]::[<Update $entity>](id, _)
                        | [<$model Event>]::[<Delete $entity>](id)
                        | [<$model Event>]::[<Pin $entity>](id)
                        | [<$model Event>]::[<Unpin $entity>](id) => id.raw(),
                    )*
                }
            }

            fn check_event(event: &Self::Event) -> ::core::result::Result<(), ::std::string::String> {
                match event {
                    $(
                        [<$model Event>]::[<Create $entity>](_, entity) => $crate::Entity::check(&**entity),
                        [<$model Event>]::[<Update $entity>](_, change) =>
                            <$entity as $crate::Entity>::check_change(change),
                        [<$model Event>]::[<Delete $entity>](_)
                        | [<$model Event>]::[<Pin $entity>](_)
                        | [<$model Event>]::[<Unpin $entity>](_) => ::core::result::Result::Ok(()),
                    )*
                }
            }

            fn first_difference(&self, other: &Self) -> ::core::option::Option<::std::string::String> {
                $(
                    for (id, entity) in &self.$entity {
                        match other.$entity.get(id) {
                            ::core::option::Option::Some(o) if o == entity => {}
                            ::core::option::Option::Some(o) => return ::core::option::Option::Some(
                                ::std::format!("{id:?} differs after a replay:\n  live: {entity:?}\n  replayed: {o:?}")),
                            ::core::option::Option::None => return ::core::option::Option::Some(
                                ::std::format!("{id:?} is missing after a replay")),
                        }
                    }
                    for id in other.$entity.keys() {
                        if !self.$entity.contains_key(id) {
                            return ::core::option::Option::Some(
                                ::std::format!("{id:?} exists after a replay but not live"));
                        }
                    }
                )*
                if self.__pinned != other.__pinned {
                    return ::core::option::Option::Some(::std::format!(
                        "the pinned ids differ after a replay:\n  live: {:?}\n  replayed: {:?}",
                        self.__pinned, other.__pinned));
                }
                ::core::option::Option::None
            }

            // Written as `"CreateUser": [id, {fields}]`, `"UpdateUser": [id, {field: op}]`
            // and `"DeleteUser": id`.
            fn serialize_event<S: $crate::__private::serde::ser::SerializeMap>(
                event: &Self::Event,
                map: &mut S,
            ) -> ::core::result::Result<(), S::Error> {
                match event {
                    $(
                        [<$model Event>]::[<Create $entity>](id, entity) => map.serialize_entry(
                            ::core::concat!("Create", ::core::stringify!($entity)),
                            &(id, $crate::__private::FieldsOf(&**entity)),
                        ),
                        [<$model Event>]::[<Update $entity>](id, c) => map.serialize_entry(
                            ::core::concat!("Update", ::core::stringify!($entity)),
                            &(id, c),
                        ),
                        [<$model Event>]::[<Delete $entity>](id) => map.serialize_entry(
                            ::core::concat!("Delete", ::core::stringify!($entity)),
                            id,
                        ),
                        [<$model Event>]::[<Pin $entity>](id) => map.serialize_entry(
                            ::core::concat!("Pin", ::core::stringify!($entity)),
                            id,
                        ),
                        [<$model Event>]::[<Unpin $entity>](id) => map.serialize_entry(
                            ::core::concat!("Unpin", ::core::stringify!($entity)),
                            id,
                        ),
                    )*
                }
            }

            fn decode_event<'de, A: $crate::__private::serde::de::MapAccess<'de>>(
                tag: &str,
                map: &mut A,
            ) -> ::core::result::Result<$crate::__private::Parsed<Self::Event>, A::Error> {
                // `IgnoredAny` is only used for removed entity types.
                #[allow(unused_imports)]
                use $crate::__private::{Parsed, serde::de::IgnoredAny};
                match tag {
                    $(
                        ::core::concat!("Create", ::core::stringify!($entity))
                        $($( | ::core::concat!("Create", ::core::stringify!($old_entity)) )+)? => {
                            let (id, entity): ($crate::Id<$entity>, $crate::__private::EntityValue<$entity>) =
                                map.next_value()?;
                            ::core::result::Result::Ok(Parsed::Event(
                                [<$model Event>]::[<Create $entity>](id, ::std::boxed::Box::new(entity.0))))
                        }
                        ::core::concat!("Update", ::core::stringify!($entity))
                        $($( | ::core::concat!("Update", ::core::stringify!($old_entity)) )+)? => {
                            let (id, change): ($crate::Id<$entity>, $crate::__private::MaybeChange<$entity>) =
                                map.next_value()?;
                            ::core::result::Result::Ok(match change.0 {
                                ::core::option::Option::Some(c) => Parsed::Event([<$model Event>]::[<Update $entity>](id, c)),
                                ::core::option::Option::None => Parsed::Skipped { id: id.raw() },
                            })
                        }
                        ::core::concat!("Delete", ::core::stringify!($entity))
                        $($( | ::core::concat!("Delete", ::core::stringify!($old_entity)) )+)? =>
                            ::core::result::Result::Ok(Parsed::Event([<$model Event>]::[<Delete $entity>](map.next_value()?))),
                        ::core::concat!("Pin", ::core::stringify!($entity))
                        $($( | ::core::concat!("Pin", ::core::stringify!($old_entity)) )+)? =>
                            ::core::result::Result::Ok(Parsed::Event([<$model Event>]::[<Pin $entity>](map.next_value()?))),
                        ::core::concat!("Unpin", ::core::stringify!($entity))
                        $($( | ::core::concat!("Unpin", ::core::stringify!($old_entity)) )+)? =>
                            ::core::result::Result::Ok(Parsed::Event([<$model Event>]::[<Unpin $entity>](map.next_value()?))),
                    )*
                    $($(
                        ::core::concat!("Create", ::core::stringify!($gone_entity))
                        | ::core::concat!("Update", ::core::stringify!($gone_entity)) => {
                            let (id, _): (u64, IgnoredAny) = map.next_value()?;
                            ::core::result::Result::Ok(Parsed::Skipped { id })
                        }
                        ::core::concat!("Delete", ::core::stringify!($gone_entity))
                        | ::core::concat!("Pin", ::core::stringify!($gone_entity))
                        | ::core::concat!("Unpin", ::core::stringify!($gone_entity)) =>
                            ::core::result::Result::Ok(Parsed::Skipped { id: map.next_value()? }),
                    )*)?
                    other => ::core::result::Result::Err($crate::__private::unknown(
                        "event",
                        other,
                        ". If its entity type was removed, add `#![removed(Name)]` to the model; \
                         if it was renamed, write `NewName (was Name) { .. }`",
                    )),
                }
            }
        }

        $(
            impl $crate::Contains<$entity> for $model {
                fn created(id: $crate::Id<$entity>, entity: $entity) -> Self::Event {
                    [<$model Event>]::[<Create $entity>](id, ::std::boxed::Box::new(entity))
                }
                fn updated(id: $crate::Id<$entity>, change: [<$entity Change>]) -> Self::Event {
                    [<$model Event>]::[<Update $entity>](id, change)
                }
                fn deleted(id: $crate::Id<$entity>) -> Self::Event {
                    [<$model Event>]::[<Delete $entity>](id)
                }
                fn pinned(id: $crate::Id<$entity>) -> Self::Event {
                    [<$model Event>]::[<Pin $entity>](id)
                }
                fn unpinned(id: $crate::Id<$entity>) -> Self::Event {
                    [<$model Event>]::[<Unpin $entity>](id)
                }
                fn table(&self) -> &$crate::__private::BTreeMap<$crate::Id<$entity>, $entity> {
                    &self.$entity
                }
                fn table_mut(&mut self) -> &mut $crate::__private::BTreeMap<$crate::Id<$entity>, $entity> {
                    &mut self.$entity
                }
            }
        )*
    }};
}
