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
///   deriving `Debug`, `Default`, `Clone` and `PartialEq`. A field declared
///   as another entity type is stored as an optional [`Id`](crate::Id), and
///   [`List`](crate::List)/[`Map`](crate::Map) of entities hold ids. See
///   [`Declared`](crate::Declared) for the full mapping.
/// - `enum UserChange { Name(String), Age(u8), Parent(Option<Id<User>>), Tags(ListOp<String>) }`:
///   one variant per field, in `CamelCase`.
/// - `enum WorldEvent { CreateUser(Id<User>), UpdateUser(Id<User>, UserChange), DeleteUser(Id<User>), CreateGroup(..), .. }`,
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
/// entity types from the same model, or `List`/`Map` of those.
#[macro_export]
macro_rules! model {
    (
        $(#[$model_attr:meta])*
        $vis:vis $model:ident {
            $(
                $(#[$entity_attr:meta])*
                $entity:ident {
                    $( $(#[$field_attr:meta])* $field:ident : $ty:ty ),* $(,)?
                }
            )*
        }
    ) => { $crate::__private::paste::paste! {

        $(
            $(#[$entity_attr])*
            #[derive(::core::fmt::Debug, ::core::default::Default, ::core::clone::Clone, ::core::cmp::PartialEq)]
            $vis struct $entity {
                $( $(#[$field_attr])* $vis $field: $crate::Stored<$ty>, )*
            }

            // When the entity is used as a field type, a reference is stored.
            impl $crate::Declared for $entity {
                type Stored = ::core::option::Option<$crate::Id<$entity>>;
            }
            impl $crate::Element for $entity {
                type Elem = $crate::Id<$entity>;
            }
            impl $crate::Key for $entity {
                type Key = $crate::Id<$entity>;
            }

            #[doc = ::core::concat!("A logged change to one field of [`", ::core::stringify!($entity), "`].")]
            #[derive(::core::fmt::Debug, ::core::clone::Clone, $crate::__private::serde::Serialize, $crate::__private::serde::Deserialize)]
            #[serde(crate = "::replayable::__private::serde")]
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
                    }
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

                fn initial(&mut self, emit: &mut dyn ::core::ops::FnMut(Self::Change)) {
                    $(
                        <$crate::Stored<$ty> as $crate::Field>::initial(
                            &mut self.$field,
                            &mut |op| emit([<$entity Change>]::[<$field:camel>](op)),
                        );
                    )*
                }

                fn apply(&mut self, change: &Self::Change) -> ::core::result::Result<(), ::std::string::String> {
                    match change {
                        $( [<$entity Change>]::[<$field:camel>](op) =>
                            <$crate::Stored<$ty> as $crate::Field>::apply(&mut self.$field, op)
                                .map_err(|e| ::std::format!("{}: {e}", ::core::stringify!($field))), )*
                    }
                }
            }
        )*

        #[doc = ::core::concat!("An event in the log of [`", ::core::stringify!($model), "`].")]
        #[derive(::core::fmt::Debug, ::core::clone::Clone, $crate::__private::serde::Serialize, $crate::__private::serde::Deserialize)]
        #[serde(crate = "::replayable::__private::serde")]
        $vis enum [<$model Event>] {
            $(
                #[doc = ::core::concat!("A [`", ::core::stringify!($entity), "`] was created.")]
                [<Create $entity>]($crate::Id<$entity>),
                #[doc = ::core::concat!("A field of a [`", ::core::stringify!($entity), "`] changed.")]
                [<Update $entity>]($crate::Id<$entity>, [<$entity Change>]),
                #[doc = ::core::concat!("A [`", ::core::stringify!($entity), "`] was deleted.")]
                [<Delete $entity>]($crate::Id<$entity>),
            )*
        }

        impl ::core::fmt::Display for [<$model Event>] {
            fn fmt(&self, f: &mut ::core::fmt::Formatter) -> ::core::fmt::Result {
                match self {
                    $(
                        Self::[<Create $entity>](id) => ::core::write!(f, "create {id:?}"),
                        Self::[<Update $entity>](id, c) => ::core::write!(f, "{id:?} {c}"),
                        Self::[<Delete $entity>](id) => ::core::write!(f, "delete {id:?}"),
                    )*
                }
            }
        }

        $(#[$model_attr])*
        #[derive(::core::default::Default)]
        #[allow(non_snake_case)]
        $vis struct $model {
            $( $entity: $crate::__private::BTreeMap<$crate::Id<$entity>, $entity>, )*
        }

        impl $crate::Model for $model {
            type Event = [<$model Event>];

            fn apply(&mut self, event: &Self::Event) -> ::core::result::Result<(), ::std::string::String> {
                match event {
                    $(
                        [<$model Event>]::[<Create $entity>](id) => match self.$entity.entry(*id) {
                            $crate::__private::btree_map::Entry::Vacant(slot) => {
                                slot.insert(::core::default::Default::default());
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
                        }
                    )*
                }
                ::core::result::Result::Ok(())
            }

            fn entity_id(event: &Self::Event) -> u64 {
                match event {
                    $(
                        [<$model Event>]::[<Create $entity>](id)
                        | [<$model Event>]::[<Update $entity>](id, _)
                        | [<$model Event>]::[<Delete $entity>](id) => id.raw(),
                    )*
                }
            }
        }

        $(
            impl $crate::Contains<$entity> for $model {
                fn created(id: $crate::Id<$entity>) -> Self::Event {
                    [<$model Event>]::[<Create $entity>](id)
                }
                fn updated(id: $crate::Id<$entity>, change: [<$entity Change>]) -> Self::Event {
                    [<$model Event>]::[<Update $entity>](id, change)
                }
                fn deleted(id: $crate::Id<$entity>) -> Self::Event {
                    [<$model Event>]::[<Delete $entity>](id)
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
