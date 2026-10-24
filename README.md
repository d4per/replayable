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
assert_eq!(log[2..], [
    "User id:1 set parent=Some(User id:2)",
    "User id:1 set age=15",
    r#"User id:1 tags.splice(0..0, ["chess", "football"])"#,
    r#"create Group id:3 { title: "The Club", members: [], roles: {} }"#,
    "Group id:3 members.splice(0..0, [User id:1, User id:2])",
    "Group id:3 members.remove(0)",
    r#"Group id:3 roles.insert(User id:2, "chair")"#,
]);

// Save and load again
let loaded = Store::<World>::from_json(&store.to_json().unwrap()).unwrap();
assert_eq!(loaded[sune], store[sune]);
assert_eq!(loaded[club], store[club]);
```

Run the full demo with `cargo run --example basic`.

For something bigger, `cargo run --release --example message_board` runs a
message board that uses the log file as its database. 100 users in parallel
threads create topics, post messages, and delete their own messages when
nobody else has posted after them. Each run replays the file at startup and
continues the same board. At the end, the file is replayed from scratch and
compared with the live state.

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

- **Creating** an entity logs its whole value, every field included, as
  one event.
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
- **Deleting** isn't done by hand. You pin the entities that should stay,
  and `collect` deletes everything they can't reach, logging a delete for
  each. So a reference held by an entity never dangles.
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
| `reply_to: Weak<Message>` | `Option<Id<Message>>` (a [weak reference](#deleting-pins-and-collect)) |
| `seen_by: List<Weak<User>>` | `List<Id<User>>` |

Entities can refer to themselves, so `parent: User` and `friends: List<User>`
inside `User` both work, and so do cycles of any length, like a `Topic`
with `messages: List<Message>` whose messages have `topic: Topic`. A
reference is stored as an id, never as a copy, so nothing follows it by
itself. A reference to an entity is always optional,
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

## Deleting: pins and `collect`

There's no `delete`. Instead, you [`pin`][`Store::pin`] the entities that
should stay even when nothing refers to them, like users. Everything a
pinned entity refers to stays as well, directly or through others.
[`Store::collect`] deletes the rest:

```rust
use replayable::{model, List, Store, Weak};

model! {
    pub Board {
        Topic { title: String, messages: List<Message> }
        Message { text: String, topic: Topic, reply_to: Weak<Message> }
    }
}

let mut store = Store::<Board>::new();
let topic = store.create(Topic { title: "Hello".into(), ..Default::default() });
store.pin(topic);

let first = store.create(Message { text: "hi".into(), topic: Some(topic), ..Default::default() });
let reply = store.create(Message { text: "hi!".into(), topic: Some(topic), reply_to: Some(first) });
store.edit(topic).messages.extend([first, reply]);
assert_eq!(store.collect(), 0);

// To delete a message, take it out of the topic.
store.edit(topic).messages.remove(0);
assert_eq!(store.collect(), 1);
assert!(!store.contains(first));
assert_eq!(store[reply].reply_to, None);

let log: Vec<String> = store.events().iter().map(|e| e.to_string()).collect();
assert_eq!(log[log.len() - 3..], [
    "Topic id:1 messages.remove(0)",
    "Message id:3 set reply_to=None",
    "delete Message id:2",
]);
```

- **Cycles are deleted too.** The topic and its messages refer to each
  other, but a cycle that nothing pinned can reach is deleted as a whole.
  `collect` traces from the pins rather than counting references.
- **Weak references** (`Weak<E>`, in a field, a `List`, or as a `Map` key or
  value) don't keep anything. When `collect` deletes what one points to,
  the field is set to `None`, or the element or entry is removed, and
  that's logged before the delete.
- **New entities are kept until `collect` runs.** Link them in, or pin
  them, before you call it, for example at the end of each request.
- **Ids kept outside the store can still go stale.** An `Id` in a local
  variable, a `HashMap` or a URL isn't seen by `collect`. After a collect,
  `store.get(id)` returns `None` and `store[id]` panics. Ids are never
  reused, so a stale id never finds another entity.
- **Ids inside your own value types** are only seen if the type overrides
  [`Value::refs`]. Without that, the entity they point to can be deleted
  while still in use.
- **`collect` panics if nothing is pinned** but there are entities, since
  it would delete them all. A log written before pins existed needs its
  roots pinned first; see [Changing the model](#changing-the-model).
- **Replay doesn't run `collect`.** The deletes it logged are replayed
  like any other event. [`Store::unpin`] lets an entity go again, and
  [`Store::is_pinned`] tells whether it's pinned.

## Saving and loading

The log is written as [JSON Lines](https://jsonlines.org/), one event per
line, each with the time it was logged:

```text
{"at":"2026-10-24T09:15:02.123Z","CreateUser":[1,{"name":"Sune","age":14,"parent":null,"tags":[]}]}
{"at":"2026-10-24T09:15:02.123Z","PinUser":1}
{"at":"2026-10-24T09:15:02.123Z","UpdateUser":[1,{"age":15}]}
{"at":"2026-10-24T09:20:41.007Z","UpdateUser":[1,{"tags":{"Splice":{"at":0,"remove":0,"insert":["chess","football"]}}}]}
{"at":"2026-10-25T18:03:59.640Z","UpdateUser":[1,{"tags":{"Remove":0}}]}
{"at":"2026-11-02T07:45:12.311Z","UnpinUser":1}
{"at":"2026-11-02T07:45:12.311Z","DeleteUser":1}
```

Fields are written by name, so their order in the model doesn't matter.
Times are ISO 8601 in UTC, to the millisecond, and are available as
[`Timestamp`]s from [`Store::timestamps`].

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

[`Store::apply_event_at`] applies events one at a time, with their times.
Use it to keep a second store in sync with one that's making the changes.

### The binary format

If the log gets big, it can be written in a compact binary format instead,
with methods like the JSON ones: [`Store::write_binary`],
[`Store::write_binary_since`] for appending, [`Store::read_binary`],
[`Store::to_binary`] and [`Store::from_binary`]. It holds the same events, and the
names of fields and entity types, so changing the model works the same way.
But each name is written in full only once per file, numbers are binary,
and each time is a small difference from the time before.

A log is usually a quarter of the size of the JSON, or less. In the message
board example it's 28%, and in the benchmark 20%. Writing and reading are
about a quarter faster; replaying the events, which both formats do, takes
much of the time.

```rust
# use replayable::{model, Store};
# model! { pub World { User { name: String } } }
# let mut store = Store::<World>::new();
# store.create(User { name: "a".into() });
let bytes = store.to_binary()?;
let loaded = Store::<World>::from_binary(&bytes)?;
assert_eq!(loaded.events(), store.events());

// Converting a JSON log is a matter of reading one and writing the other.
let json = Store::<World>::from_json(&store.to_json()?)?;
assert_eq!(json.to_binary()?, bytes);
# Ok::<(), Box<dyn std::error::Error>>(())
```

Appending works as with JSON: pass the number of events already in the
file. The store remembers the names the file defines, from writing or
reading it, so they aren't written again. JSON is easier to read and edit
by hand, and to process with other tools; the binary format needs this
crate (or its [layout](#binary-layout)).

### Going back in time

Nothing is ever removed from the log. An edit or a `collect` only adds
events, so a deleted entity's creation and every change to it stay in the
log, followed by its deletion. [`Store::as_of`] rebuilds the store as it was
after the first `n` events, for example just before something was deleted:

```rust
# use replayable::{model, Store};
# model! { pub World { User { name: String } } }
let mut store = Store::<World>::new();
let me = store.create(User { name: "me".into() });
store.pin(me);
let u = store.create(User { name: "a".into() });
let before = store.events().len();
store.collect();

assert!(!store.contains(u));
assert_eq!(store.as_of(before)[u].name, "a");
```

[`Store::as_of_time`] does the same by time: the store after every event
logged at or before a given moment.

```rust
# use replayable::{model, Store, Timestamp};
# model! { pub World { User { name: String } } }
# let mut store = Store::<World>::new();
let monday: Timestamp = "2026-10-26T00:00:00.000Z".parse()?;
let then = store.as_of_time(monday);
# Ok::<(), replayable::ParseTimestampError>(())
```

Several events can be logged in the same millisecond, so to get to an exact
point, like "just before this delete", use positions with `as_of`.

## Changing the model

A log outlives the code that wrote it. These changes keep old logs
readable:

```rust
replayable::model! {
    pub World {
        #![removed(Team)]                        // entity types that no longer exist
        User {
            #![removed(age, status)]             // fields that no longer exist
            name: String,
            level: u8 = 1,                       // a new field, and its value for old users
            nickname (was nick): Option<String>, // a renamed field
        }
        Person (was Human) { title: String }     // a renamed entity type
    }
}
```

| Change | What happens to old logs |
|---|---|
| Add a field | Entities created before it existed get its default: the `= value` after the type, or the type's `Default`. |
| Change a field's default | Only entities created before the field existed get the new default. Everything created later has its value in the log, since `create` logs every field. |
| Remove a field or entity type | List it in `#![removed(..)]`, and its old events are skipped. **Without that, loading fails** with an error that says what to add. |
| Rename a field or entity type | Write `new (was old)`; old events apply to the new name. Several old names: `(was a, b)`. |
| Reorder fields or entities | Nothing; fields are stored by name. |
| Widen a type (`u8` → `u64`, `T` → `Option<T>`) | Old values are read into the new type. |
| Change a type so old values don't fit (`u8` → `String`) | Loading fails with [`Error::Json`] and the line number. |
| Change what a reference points to (`parent: User` → `parent: Group`) | **Not detected.** The old ids are read as ids of the new type. Add a new field instead. |
| Re-add a name that was removed | Its old values come back, since they're still in the log. Pick a new name instead. |
| Change your own value types | Use serde's attributes on them: `#[serde(default)]` for new fields, `#[serde(alias = "old")]` for renames. |
| Make a reference weak, or strong again (`reply_to: Message` ↔ `Weak<Message>`) | Nothing: both are stored as ids. The next `collect` follows the new rule. |

Using the same name twice (say, as a field and as removed) is a compile
error. Skipped events aren't kept in [`Store::events`], so writing the whole
log again with [`Store::write_json`] drops them for good. A rewrite also
writes every field of every create, which fixes the defaults the log was
read with. Appending with `write_json_since` keeps the old lines as they are.

A default (`= value`) is evaluated each time an old entity is replayed, so
make it a constant, not something like the current time.

A log written before pins existed has none, and `collect` refuses to run on
it. Pin the roots once after loading it; `pin` returns `false` for those
already pinned, so this can run at every start:

```rust
# use replayable::{model, Id, Store};
# model! { pub World { User { name: String } } }
# let mut store = Store::<World>::new();
let users: Vec<Id<User>> = store.all::<User>().map(|(id, _)| id).collect();
for u in users {
    store.pin(u);
}
```

## What the compiler can't check

Some mistakes compile fine but would make the log wrong. Most of them are
caught when the change is made, rather than when the log is loaded months
later:

| Mistake | Caught by |
|---|---|
| NaN or infinity in an `f32`/`f64` field, list or map (JSON writes them as `null`, which can't be read back) | [`Value::check`]: the edit panics. Override `check` in your own types that hold floats. |
| A value type whose serde loses data (`#[serde(skip)]`, a lossy format), or that can't be written as JSON (a map with non-string keys) | Debug builds: every logged change is written as JSON, read back and compared, and the edit panics if they differ. |
| State changed without an edit, through a `Cell` or `RefCell` in a value | [`Store::verify`], which replays the log and compares it with the live state. Call it in your tests. |
| A `PartialEq` that ignores some fields | Nothing. A change to only those fields isn't seen, so it isn't logged. |
| `usize`/`isize` fields, read on a platform with a different width | Loading fails if a value doesn't fit. Prefer fixed-width integers. |
| An `Ord` on a map key type that disagrees with its `PartialEq` | Nothing. Keep them consistent, as `BTreeMap` requires anyway. |
| An `Id` inside your own value type, without overriding [`Value::refs`] | Nothing. `collect` doesn't see the id, and can delete what it points to while it's still in use. |

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
| `store.create(value)` | a clone of the value | one `create` event holding every field |
| `store.pin(id)`, `unpin` | O(log entities) | one `pin`/`unpin` event, if it changed |
| `store.collect()` | O(entities + references), and an edit of each entity whose weak references are cleared | the weak changes, then one `delete` per entity |

Rough numbers from `cargo bench` on a laptop: an `edit` that changes a
scalar field takes about 80 ns, a `push` in an edit of its own about 45 ns,
replay about 60 ns per event (230 ns from JSON), and `collect` about
150 ns per entity.

## Things to know

- **The guard borrows the whole store.** You can't read another entity
  through `store` while an `Edit` is alive. Read first, or use
  [`Store::update`], whose closure gets `&Store`. Reading the entity being
  edited through that `&Store` panics with a clear message; use the
  `&mut` you were given.
- **`update` can't create or edit other entities.** Its closure only gets
  `&Store`. Make those changes before or after.
- **Only `collect` deletes**, and only what no pinned entity can reach, so
  the references entities hold never dangle. Ids you keep outside the
  store can; see [Deleting](#deleting-pins-and-collect).
- **Ids are unique across all entity types** in a store and are never
  reused. Use [`Id::from_raw`] to rebuild one from a number; it isn't
  checked.
- **The whole log stays in memory**, and loading replays it from the start.
  [`Store::read_binary`] reads the whole file into memory before replaying.
  There are no snapshots or compaction yet.
- **Don't `mem::forget` an `Edit`.** The changes stay in the entity but are
  never logged, so a replay won't match.
- **Times come from the system clock** and never go backwards in the log:
  if the clock is set back, events get the time of the event before them.
  [`Store::with_clock`] sets another clock, for example a fixed one in
  tests.
- **Single writer.** A `Store` is a plain value. Share it the way you would
  share any other one (`Mutex`, a single owner task, ...).

## Event format

The JSON written by [`Store::write_json`] is what you'll have on disk, so
it's treated as a contract, and a test pins it down. Before 1.0 it may still
change between minor versions (0.1 → 0.2), and any change is listed in the
[changelog](CHANGELOG.md). Changes to your own model are covered in
[Changing the model](#changing-the-model). The same goes for the binary
format.

### Binary layout

Like the JSON format, it's self-describing: fields and enum variants are
written by name, so logs written by an older version of a model can be
read the same way. To keep it small, each name is written in full only
the first time; after that it's a number, usually one byte.

```text
log    = "RPLB" 0x01 record*      (an empty file is an empty log)
record = varint d, then a value   d > 0: an event, logged d - 1 ms after the one before
                                  d = 0: reset, which forgets all names, and the next
                                         time counts from 1970 again
```

The first event's time counts from 1970. A varint is unsigned LEB128:
seven bits per byte, lowest first, with the top bit set on all but the
last byte. The value of an event is the same as in JSON, without the time:
`{"CreateUser": [id, {fields}]}` and so on.

A value is one tag byte, then its data:

| Tag | Value |
|---|---|
| `0x00` | unit, `None` |
| `0x01`, `0x02` | `false`, `true` |
| `0x03` | `Some`, then the value |
| `0x04` | an unsigned integer: varint (up to 128 bits) |
| `0x05` | a negative integer `n`: varint of `-(n + 1)` |
| `0x06`, `0x07` | `f32`, `f64`, little-endian |
| `0x08` | a string: varint length, then UTF-8 |
| `0x09` | bytes: varint length, then the bytes |
| `0x0A`, `0x0B` | a sequence, a map: varint count, then the items (keys and values in turn) |
| `0x0C` | a name: varint length, then UTF-8. Used as a string, and gets the next number |
| `0x0D` | a name defined earlier: varint number |
| `0x0E`, `0x0F` | a sequence, a map, of items up to `0x10` |
| `0x10` | the end of a sequence or map that started with `0x0E` or `0x0F` |
| `0x20`–`0x4F` | a name defined earlier, number 0–47 |
| `0x50`–`0x5F` | a sequence of 0–15 items |
| `0x60`–`0x6F` | a map of 0–15 entries |
| `0x70`–`0x7F` | a string of 0–15 bytes |
| `0x80`–`0xFF` | an unsigned integer 0–127 |

Struct field names, enum variant names and map keys that are short
strings are written as names. Enums are written like serde_json does: a
unit variant as its name, others as a map with one entry, from the name
to the content. Numbers are numbers whatever their type, so `u8 → u32`
still reads old values, as in JSON. A value that isn't `Some` or `None`
is read as `Some` where an `Option` is expected.

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
[`Store::write_json`]: https://docs.rs/replayable/latest/replayable/struct.Store.html#method.write_json
[`Error::Json`]: https://docs.rs/replayable/latest/replayable/enum.Error.html#variant.Json
[`Value::check`]: https://docs.rs/replayable/latest/replayable/trait.Value.html#method.check
[`Value::refs`]: https://docs.rs/replayable/latest/replayable/trait.Value.html#method.refs
[`Store::pin`]: https://docs.rs/replayable/latest/replayable/struct.Store.html#method.pin
[`Store::unpin`]: https://docs.rs/replayable/latest/replayable/struct.Store.html#method.unpin
[`Store::is_pinned`]: https://docs.rs/replayable/latest/replayable/struct.Store.html#method.is_pinned
[`Store::collect`]: https://docs.rs/replayable/latest/replayable/struct.Store.html#method.collect
[`Store::verify`]: https://docs.rs/replayable/latest/replayable/struct.Store.html#method.verify
[`Store::events`]: https://docs.rs/replayable/latest/replayable/struct.Store.html#method.events
[`Store::as_of`]: https://docs.rs/replayable/latest/replayable/struct.Store.html#method.as_of
[`Store::write_binary`]: https://docs.rs/replayable/latest/replayable/struct.Store.html#method.write_binary
[`Store::write_binary_since`]: https://docs.rs/replayable/latest/replayable/struct.Store.html#method.write_binary_since
[`Store::to_binary`]: https://docs.rs/replayable/latest/replayable/struct.Store.html#method.to_binary
[`Store::from_binary`]: https://docs.rs/replayable/latest/replayable/struct.Store.html#method.from_binary
[`Store::read_binary`]: https://docs.rs/replayable/latest/replayable/struct.Store.html#method.read_binary
[`Store::as_of_time`]: https://docs.rs/replayable/latest/replayable/struct.Store.html#method.as_of_time
[`Store::apply_event_at`]: https://docs.rs/replayable/latest/replayable/struct.Store.html#method.apply_event_at
[`Store::timestamps`]: https://docs.rs/replayable/latest/replayable/struct.Store.html#method.timestamps
[`Store::with_clock`]: https://docs.rs/replayable/latest/replayable/struct.Store.html#method.with_clock
[`Timestamp`]: https://docs.rs/replayable/latest/replayable/struct.Timestamp.html
