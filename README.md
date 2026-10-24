# replayable

Plain Rust structs whose every change is recorded as an event.

You declare your entities once. You then create, edit and read them as
ordinary structs: assign fields, push to lists, `sort`, `retain`, use
`entry` on maps. Every change is captured as a small, readable event, with
no command types, no reducers and no hand-written diffing. The event log
can be saved as JSON Lines and replayed into exactly the same state.

```rust
use replayable::{model, List, Map, Store};

model! {
    pub World {
        User {
            name: String,
            age: u8,
            parent: User,          // a reference to another User
            tags: List<String>,    // the whole Vec API, every change logged
        }
        Group {
            title: String,
            members: List<User>,       // a list of references
            roles: Map<User, String>,  // a map with references as keys
        }
    }
}

let mut store = Store::<World>::new();

// Create with a plain struct literal
let sune = store.create(User { name: "Sune".into(), age: 14, ..Default::default() });
let bertil = store.create(User { name: "Bertil".into(), age: 42, ..Default::default() });

// Edit through a plain `&mut User`
store.edit(sune).parent = Some(bertil);
{
    let mut u = store.edit(sune);
    u.age += 1;
    u.tags.push("chess".into());
    u.tags.push("football".into());
}

// Read other entities while editing
let club = store.create(Group { title: "The Club".into(), ..Default::default() });
store.edit(club).members.extend([sune, bertil]);
store.update(club, |g, store| g.members.retain(|&m| store[m].age >= 18));
store.edit(club).roles.insert(bertil, "chair".into());

// Read: store[id] gives a plain &User
assert_eq!(store[store[sune].parent.unwrap()].name, "Bertil");

// Every change is in the log
let log: Vec<String> = store.events().iter().map(|e| e.to_string()).collect();
assert_eq!(log[6..], [
    "User id:1 set parent=Some(User id:2)",
    "User id:1 set age=15",
    r#"User id:1 tags.splice(0..0, ["chess", "football"])"#,
    "create Group id:3",
    r#"Group id:3 set title="The Club""#,
    "Group id:3 members.splice(0..0, [User id:1, User id:2])",
    "Group id:3 members.remove(0)",
    r#"Group id:3 roles.insert(User id:2, "chair")"#,
]);

// Save and load again
let loaded = Store::<World>::from_json(&store.to_json()).unwrap();
assert_eq!(loaded[sune], store[sune]);
assert_eq!(loaded[club], store[club]);
```

Run the full demo with `cargo run --example basic`.

## Why

Event sourcing gives you an audit trail, undo, sync between processes and
"what did this look like last Tuesday" for free. The usual price is
ceremony: a command type and an event type per operation, plus a reducer
that has to stay in sync with the code that makes the changes.

`replayable` gets rid of that ceremony. The events are derived from the
changes you make to plain structs, and replaying them goes through the
same types. A test in this crate makes thousands of random changes of every
kind and checks that a replay always gives the same state.

## How it works

`model!` generates a plain struct per entity, a `<Entity>Change` enum with
one variant per field, and a `<Model>Event` enum with
`Create`/`Update`/`Delete` variants per entity.

- **Scalar fields** (strings, numbers, enums ...) are snapshotted when an
  edit starts and compared when it ends. A field that changed is logged as
  `set field=value`.
- **`List` and `Map`** track their own changes. `push`, `insert`, `remove`,
  `retain`, `list[i] = x` and the like are recorded as they happen.
  Adjacent changes made in one edit are merged, so `extend` with 1000 items
  is one event. The rest of the `Vec`/`BTreeMap` API works through
  `DerefMut`: the collection is copied first, and the change is found with a
  diff when the edit ends.
- **Replacing** a whole list or map (`g.members = List::new()`, or
  `*user = User { .. }`) is noticed. Every collection carries a unique
  number, and the new contents are logged.
- **Replay** applies events one by one to an empty model. It checks that
  every event fits, and returns an [`Error`] instead of panicking on a corrupt
  log.

Edits go through [`Store::edit`], which returns a guard that derefs to
`&mut Entity` and turns the changes into events when it's dropped. Use
[`Store::update`] when the edit also needs to read other entities.

## Field types

| You write                | It's stored as           |
|--------------------------|--------------------------|
| `name: String`           | `String`                 |
| `age: u8`, `score: f64`  | the same type            |
| `nick: Option<String>`   | `Option<String>`         |
| `status: Status` (your own [`Value`] type) | `Status` |
| `parent: User`           | `Option<Id<User>>`       |
| `tags: List<String>`     | [`List<String>`][List]   |
| `members: List<User>`    | `List<Id<User>>`         |
| `roles: Map<User, Role>` | [`Map<Id<User>, Role>`][Map] |

Entities can refer to themselves, so `parent: User` and `friends: List<User>`
inside `User` both work. A reference to an entity is always optional,
because a new entity starts out from `Default`.

Use `List` and `Map` instead of `Vec` and `BTreeMap`. They have the same API,
but changes made directly to a plain `Vec` can't be seen.

### Your own value types

Any type that is `Clone + PartialEq + Debug + Serialize + Deserialize` can be
a field once you opt in with an empty impl. It also needs `Default` to be a
field directly:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
enum Status {
    #[default]
    Active,
    Away { until: String },
}
impl replayable::Value for Status {}
```

A change to such a field is logged as the whole new value, so keep value
types small. Anything with its own identity or a long life should be an
entity.

`replayable` uses `serde` itself. You only need it in your own
`Cargo.toml` for deriving your own value types, as above.

### Visibility and attributes

The visibility before the model name (`pub World` above) applies to every
generated type and field. Leave it out to keep them private to the module.
Doc comments and other attributes can be put on the model, on entities and
on fields, and they end up on the generated items.

## Saving and loading

The log is written as [JSON Lines](https://jsonlines.org/), one event per
line:

```text
{"CreateUser":1}
{"UpdateUser":[1,{"Name":"Sune"}]}
{"UpdateUser":[1,{"Tags":{"Splice":{"at":0,"remove":0,"insert":["chess","football"]}}}]}
{"UpdateUser":[1,{"Tags":{"Remove":0}}]}
```

Since the log only grows, you can append to a file instead of rewriting it:

```rust,no_run
# use replayable::{model, Store};
# model! { pub World { User { name: String } } }
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter};

let mut store = Store::<World>::read_json(BufReader::new(File::open("log.jsonl")?))?;
let mut saved = store.events().len();

// ... make changes ...

let file = OpenOptions::new().append(true).open("log.jsonl")?;
store.write_json_since(saved, BufWriter::new(file))?;
saved = store.events().len();
# Ok::<(), Box<dyn std::error::Error>>(())
```

[`Store::apply_event`] applies events one at a time. Use it to keep a
second store in sync with one that's making the changes.

## What things cost

`n` is the length of the list or map, and `k` is the number of elements
that changed.

| Operation | Extra cost from tracking | Logged |
|---|---|---|
| `store.edit(id)` | a clone of every scalar field (Strings too) to compare against; `List`/`Map` fields cost nothing | one event per change |
| assigning a scalar field | a comparison when the edit ends | `set field=value` if it changed |
| `List::push`, `insert`, `remove`, `pop`, `set`, `swap`, `get_mut`, `list[i]` | O(1) + a clone of the value | one op; adjacent ops in one edit are merged |
| `List::extend`, `truncate`, `retain` | O(k) / O(n) | one op per run of adjacent changes |
| `List` through `DerefMut` (`sort`, `iter_mut`, `drain`, `dedup` ...) | a copy of the list and a diff: O(n) for the unchanged start and end, O(m²) for the changed middle part of length m. Above about a million pairs, the middle part is logged as one splice instead | minimal inserts and removes |
| `Map::insert`, `remove`, `get_mut`, `retain` | O(log n) + a clone | one op per key |
| `Map` through `DerefMut` (`entry` ...) | a copy of the map and a diff, O(n) | one op per changed key |
| `store.update(id, ..)` | like `edit`, plus taking the entity out of the table and putting it back, O(log entities) | like `edit` |
| replacing a whole `List`/`Map` | – | `clear` + the full contents |
| `store.create(value)` | – | `create` + one event per non-default field |

Rough numbers from `cargo bench` on a laptop: an `edit` that changes a
scalar field takes about 80 ns, a `push` in an edit of its own about 45 ns,
and replay about 60 ns per event (170 ns from JSON).

## Things to know

- **The guard borrows the whole store.** You can't read another entity
  through `store` while an `Edit` is alive. Read first, or use
  [`Store::update`], whose closure gets `&Store`. Reading the entity being
  edited through that `&Store` panics with a clear message; use the
  `&mut` you were given.
- **`update` can't create or edit other entities.** Its closure only gets
  `&Store`. Make those changes before or after.
- **Deleting doesn't clean up references.** `Id`s that pointed to a deleted
  entity are left dangling: `store[id]` panics and `store.get(id)` returns
  `None`.
- **Ids are unique across all entity types** in a store and are never
  reused. Use [`Id::from_raw`] to rebuild one from a number; it isn't
  checked.
- **The whole log stays in memory**, and loading replays it from the start.
  There are no snapshots or compaction yet.
- **Don't `mem::forget` an `Edit`.** The changes stay in the entity but are
  never logged, so a replay won't match.
- **Interior mutability isn't seen.** A `Cell` inside a value can change
  without an edit, and that change is never logged.
- **Single writer.** A `Store` is a plain value. Share it the way you would
  share any other one (`Mutex`, a single owner task, ...).
- **Don't rename the dependency.** `model!` refers to the crate as
  `::replayable` in one place, because serde needs a fixed path.

## Event format

The JSON written by [`Store::write_json`] is what you'll have on disk, so
it's treated as a contract, and a test pins it down. Before 1.0 it may still
change between minor versions (0.1 → 0.2), and any change is listed in the
[changelog](CHANGELOG.md). Renaming an entity or a field renames its events,
so old logs won't load afterwards.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this crate by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.

[`Error`]: https://docs.rs/replayable/latest/replayable/enum.Error.html
[`Value`]: https://docs.rs/replayable/latest/replayable/trait.Value.html
[List]: https://docs.rs/replayable/latest/replayable/struct.List.html
[Map]: https://docs.rs/replayable/latest/replayable/struct.Map.html
[`Id::from_raw`]: https://docs.rs/replayable/latest/replayable/struct.Id.html#method.from_raw
[`Store::edit`]: https://docs.rs/replayable/latest/replayable/struct.Store.html#method.edit
[`Store::update`]: https://docs.rs/replayable/latest/replayable/struct.Store.html#method.update
[`Store::apply_event`]: https://docs.rs/replayable/latest/replayable/struct.Store.html#method.apply_event
[`Store::write_json`]: https://docs.rs/replayable/latest/replayable/struct.Store.html#method.write_json
