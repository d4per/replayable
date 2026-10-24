use crate::binary;
use crate::codec::{self, Parsed};
use crate::{Error, Id, Timestamp};
use serde::de::MapAccess;
use serde::ser::{SerializeMap, Serializer};
use serde::{de::DeserializeOwned, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::io::{BufRead, Read, Write};
use std::ops::{Deref, DerefMut, Index};
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Mutex, PoisonError};

/// An entity type declared in [`model!`](crate::model). Implemented by the
/// macro; you don't implement it yourself.
pub trait Entity: Default + Clone + PartialEq + fmt::Debug + 'static {
    /// The name of the type, used in `Debug` output of ids.
    const KIND: &'static str;
    /// A change to one field, generated as `<Entity>Change`.
    type Change: Clone + PartialEq + fmt::Debug + fmt::Display + Serialize + DeserializeOwned;
    /// What's needed to see what changed during an edit.
    type Snapshot;
    /// Taken before an edit.
    fn snapshot(&self) -> Self::Snapshot;
    /// Emits the changes made since `before` was taken.
    fn changes(&mut self, before: Self::Snapshot, emit: &mut dyn FnMut(Self::Change));
    /// Forgets changes not yet emitted in all `List` and `Map` fields. Used by
    /// `create`, which logs the whole value instead.
    fn settle(&mut self);
    /// Applies a logged change during replay.
    fn apply(&mut self, change: &Self::Change) -> Result<(), String>;
    /// Checks every value in the entity with [`Value::check`](crate::Value::check).
    fn check(&self) -> Result<(), String>;
    /// Checks every value in a change with [`Value::check`](crate::Value::check).
    fn check_change(change: &Self::Change) -> Result<(), String>;
    /// Writes the entity as a map from field name to value.
    #[doc(hidden)]
    fn serialize_fields<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error>;
    /// Reads the value of one field from a map, by its current name, an old
    /// name, or a removed name (whose value is skipped).
    #[doc(hidden)]
    fn decode_field<'de, A: MapAccess<'de>>(&mut self, field: &str, map: &mut A) -> Result<(), A::Error>;
    /// Reads a change to one field from a map. `None` if the field was removed.
    #[doc(hidden)]
    fn decode_change<'de, A: MapAccess<'de>>(field: &str, map: &mut A) -> Result<Option<Self::Change>, A::Error>;
}

/// A whole model declared with [`model!`](crate::model): one table per
/// entity type, and the event type for all of them. Implemented by the
/// macro; you don't implement it yourself.
pub trait Model: Default {
    /// The event type, generated as `<Model>Event`.
    type Event: Clone + PartialEq + fmt::Debug + fmt::Display + Serialize + DeserializeOwned;
    /// Applies an event during replay. Fails if it doesn't fit the state.
    fn apply(&mut self, event: &Self::Event) -> Result<(), String>;
    /// The raw id of the entity the event is about.
    fn entity_id(event: &Self::Event) -> u64;
    /// Checks every value in an event with [`Value::check`](crate::Value::check).
    fn check_event(event: &Self::Event) -> Result<(), String>;
    /// Describes the first entity that differs between two states, if any.
    fn first_difference(&self, other: &Self) -> Option<String>;
    /// Writes an event as one entry of a map, like `"CreateUser": [..]`.
    #[doc(hidden)]
    fn serialize_event<S: SerializeMap>(event: &Self::Event, map: &mut S) -> Result<(), S::Error>;
    /// Reads an event with the tag `tag` (like `CreateUser`) from a map.
    #[doc(hidden)]
    fn decode_event<'de, A: MapAccess<'de>>(tag: &str, map: &mut A) -> Result<Parsed<Self::Event>, A::Error>;
}

/// Says that the model `Self` has a table of `E`. Implemented by
/// [`model!`](crate::model) for every entity in the model, and used as the
/// bound on [`Store`] methods: `M: Contains<E>`.
pub trait Contains<E: Entity>: Model {
    /// The event for creating `id` with the value `entity`.
    fn created(id: Id<E>, entity: E) -> Self::Event;
    /// The event for a change to `id`.
    fn updated(id: Id<E>, change: E::Change) -> Self::Event;
    /// The event for deleting `id`.
    fn deleted(id: Id<E>) -> Self::Event;
    /// The table of `E`.
    fn table(&self) -> &BTreeMap<Id<E>, E>;
    /// The table of `E`, mutably.
    fn table_mut(&mut self) -> &mut BTreeMap<Id<E>, E>;
}

/// Where the times of live events come from.
type Clock = Arc<dyn Fn() -> Timestamp + Send + Sync>;

/// The events, each with the time it was logged.
struct Log<M: Model> {
    events: Vec<M::Event>,
    times: Vec<Timestamp>,
    clock: Clock,
}

impl<M: Model> Log<M> {
    /// The time for an event logged now: the clock's time, but never before
    /// the event before it, so that the times only go forward even if the
    /// system clock is set back.
    fn now(&self) -> Timestamp {
        let now = (self.clock)();
        self.times.last().map_or(now, |&last| now.max(last))
    }

    /// Adds a live event, after checking that it can be stored. In debug
    /// builds it's also checked that it survives a trip through JSON.
    fn push_live(&mut self, event: M::Event) {
        if let Err(reason) = M::check_event(&event) {
            codec::reject(format_args!("can't log `{event}`: {reason}"));
        }
        #[cfg(debug_assertions)]
        codec::assert_round_trip::<M>(&event);
        let at = self.now();
        self.push(at, event);
    }

    fn push(&mut self, at: Timestamp, event: M::Event) {
        self.events.push(event);
        self.times.push(at);
    }
}

/// The current state of a model, and the log of events that led to it.
///
/// The state changes in two ways, which always give the same result:
/// - Live: entities are changed directly, and the changes are logged
///   ([`create`](Store::create), [`edit`](Store::edit),
///   [`update`](Store::update), [`delete`](Store::delete)).
/// - Replay: events are applied one by one ([`replay`](Store::replay),
///   [`read_json`](Store::read_json), [`apply_event`](Store::apply_event)).
///
/// Reading works like a map: `store[id]` gives a `&E` and panics if the id
/// doesn't exist, [`get`](Store::get) gives an `Option<&E>`.
///
/// Every event is logged with the time it happened, a [`Timestamp`] to the
/// millisecond. See [`timestamps`](Store::timestamps) and
/// [`as_of_time`](Store::as_of_time).
///
/// ```
/// use replayable::{model, List, Store};
///
/// model! {
///     pub Shop {
///         Item { name: String, price: u32 }
///         Cart { items: List<Item>, total: u32 }
///     }
/// }
///
/// let mut store = Store::<Shop>::new();
/// let pen = store.create(Item { name: "Pen".into(), price: 3 });
/// let cart = store.create(Cart::default());
///
/// store.edit(cart).items.push(pen);
/// store.edit(pen).price = 4;
///
/// // `update` lets the closure read the rest of the store.
/// store.update(cart, |c, store| c.total = c.items.iter().map(|&i| store[i].price).sum());
/// assert_eq!(store[cart].total, 4);
///
/// let replayed = Store::<Shop>::replay(store.events().to_vec()).unwrap();
/// assert_eq!(replayed[cart], store[cart]);
/// ```
pub struct Store<M: Model> {
    state: M,
    log: Log<M>,
    next_id: u64,
    /// The raw id of the entity checked out by `update`, for a clear panic
    /// message if the closure tries to read it through the store.
    editing: Option<u64>,
    /// What's needed to append to the binary file last written or read.
    binary: Mutex<binary::WriteState>,
}

impl<M: Model> Default for Store<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: Model> Store<M> {
    /// An empty store.
    pub fn new() -> Self {
        let log = Log { events: Vec::new(), times: Vec::new(), clock: Arc::new(Timestamp::now) };
        Store { state: M::default(), log, next_id: 1, editing: None, binary: Mutex::default() }
    }

    /// Uses `clock` instead of the system clock for the times of new events.
    /// Useful in tests, to get the same log every time.
    ///
    /// The times in the log never go backwards: an event gets the time of
    /// the event before it if the clock says it's earlier.
    ///
    /// ```
    /// use replayable::{model, Store, Timestamp};
    /// use std::sync::atomic::{AtomicU64, Ordering};
    ///
    /// model! { pub M { Note { text: String } } }
    ///
    /// // A clock that starts in 2026 and ticks one second per event.
    /// let ticks = AtomicU64::new(0);
    /// let mut store = Store::<M>::new().with_clock(move || {
    ///     Timestamp::from_unix_millis(1_792_833_302_000 + 1000 * ticks.fetch_add(1, Ordering::Relaxed))
    /// });
    /// let note = store.create(Note::default());
    /// store.edit(note).text = "hi".into();
    /// assert_eq!(store.timestamps()[1].to_string(), "2026-10-24T09:15:03.000Z");
    /// ```
    pub fn with_clock(mut self, clock: impl Fn() -> Timestamp + Send + Sync + 'static) -> Self {
        self.log.clock = Arc::new(clock);
        self
    }

    /// Creates an entity from a plain struct value and returns its id.
    ///
    /// The whole value is logged, every field included. So if a field's
    /// default changes later, entities created before that keep the values
    /// they were created with.
    pub fn create<E: Entity>(&mut self, mut value: E) -> Id<E>
    where
        M: Contains<E>,
    {
        let id = Id::from_raw(self.next_id);
        self.next_id += 1;
        value.settle();
        self.log.push_live(M::created(id, value.clone()));
        self.state.table_mut().insert(id, value);
        id
    }

    /// Gives a plain `&mut E` (through a guard). When the guard is dropped,
    /// the changes become events.
    ///
    /// Panics if `id` doesn't exist. See [`try_edit`](Store::try_edit).
    pub fn edit<E: Entity>(&mut self, id: Id<E>) -> Edit<'_, M, E>
    where
        M: Contains<E>,
    {
        self.try_edit(id).unwrap_or_else(|| panic!("{id:?} doesn't exist"))
    }

    /// Like [`edit`](Store::edit), but returns `None` if `id` doesn't exist.
    pub fn try_edit<E: Entity>(&mut self, id: Id<E>) -> Option<Edit<'_, M, E>>
    where
        M: Contains<E>,
    {
        // The entity stays where it is. The guard borrows it and the log,
        // which are separate fields, so nothing is moved or copied.
        let entity = self.state.table_mut().get_mut(&id)?;
        let before = Some(entity.snapshot());
        Some(Edit { entity, log: &mut self.log, id, before })
    }

    /// Like [`edit`](Store::edit), but the closure can also read the rest of
    /// the store. Returns what the closure returns.
    ///
    /// The entity being edited is taken out of the store while the closure
    /// runs, so reading it through the store panics; use the `&mut E` you
    /// were given instead. If the closure panics, the entity is put back and
    /// the changes made so far are logged.
    ///
    /// Panics if `id` doesn't exist. See [`try_update`](Store::try_update).
    pub fn update<E: Entity, R>(&mut self, id: Id<E>, f: impl FnOnce(&mut E, &Self) -> R) -> R
    where
        M: Contains<E>,
    {
        self.try_update(id, f).unwrap_or_else(|| panic!("{id:?} doesn't exist"))
    }

    /// Like [`update`](Store::update), but returns `None` if `id` doesn't exist.
    pub fn try_update<E: Entity, R>(&mut self, id: Id<E>, f: impl FnOnce(&mut E, &Self) -> R) -> Option<R>
    where
        M: Contains<E>,
    {
        let mut entity = self.state.table_mut().remove(&id)?;
        self.editing = Some(id.raw());
        let before = entity.snapshot();
        // Even if the closure panics, the entity must go back into the store.
        let result = panic::catch_unwind(AssertUnwindSafe(|| f(&mut entity, &*self)));
        let log = &mut self.log;
        entity.changes(before, &mut |c| log.push_live(M::updated(id, c)));
        self.state.table_mut().insert(id, entity);
        self.editing = None;
        Some(result.unwrap_or_else(|payload| panic::resume_unwind(payload)))
    }

    /// Deletes an entity and returns it, or `None` if it didn't exist.
    ///
    /// Other entities that refer to it keep their `Id`s, which then point
    /// at nothing: `store[id]` panics and `store.get(id)` returns `None`.
    pub fn delete<E: Entity>(&mut self, id: Id<E>) -> Option<E>
    where
        M: Contains<E>,
    {
        let entity = self.state.table_mut().remove(&id)?;
        self.log.push_live(M::deleted(id));
        Some(entity)
    }

    /// The entity with this id, if it exists.
    pub fn get<E: Entity>(&self, id: Id<E>) -> Option<&E>
    where
        M: Contains<E>,
    {
        self.state.table().get(&id)
    }

    /// Whether an entity with this id exists.
    pub fn contains<E: Entity>(&self, id: Id<E>) -> bool
    where
        M: Contains<E>,
    {
        self.state.table().contains_key(&id)
    }

    /// The number of entities of type `E`.
    pub fn count<E: Entity>(&self) -> usize
    where
        M: Contains<E>,
    {
        self.state.table().len()
    }

    /// All entities of a type, sorted by id.
    pub fn all<E: Entity>(&self) -> impl DoubleEndedIterator<Item = (Id<E>, &E)> + ExactSizeIterator
    where
        M: Contains<E>,
    {
        self.state.table().iter().map(|(id, e)| (*id, e))
    }

    /// The whole log, oldest event first.
    pub fn events(&self) -> &[M::Event] {
        &self.log.events
    }

    /// The time of every event in [`events`](Store::events), in the same
    /// order. They never go backwards.
    ///
    /// ```
    /// # use replayable::{model, Store};
    /// # model! { pub M { Note { text: String } } }
    /// # let mut store = Store::<M>::new();
    /// # store.create(Note::default());
    /// for (at, event) in store.timestamps().iter().zip(store.events()) {
    ///     println!("{at} {event}"); // 2026-10-24T09:15:02.123Z create Note id:1 { text: "" }
    /// }
    /// ```
    pub fn timestamps(&self) -> &[Timestamp] {
        &self.log.times
    }

    /// The events after the first `n`. Save `events().len()` after writing
    /// the log, and pass it here next time to get only the new events.
    ///
    /// Panics if `n` is larger than the number of events.
    pub fn events_since(&self, n: usize) -> &[M::Event] {
        &self.log.events[n..]
    }

    /// The store as it was after the first `n` events, history included.
    ///
    /// Nothing is ever removed from the log: an edit or a delete only adds
    /// events. So any earlier state can be rebuilt, for example the one just
    /// before something was deleted. The current store isn't changed.
    ///
    /// The events keep their times, and the new store has the same clock.
    ///
    /// Panics if `n` is larger than the number of events.
    pub fn as_of(&self, n: usize) -> Self {
        let mut store = Self::new();
        store.log.clock = self.log.clock.clone();
        for (&at, event) in self.log.times[..n].iter().zip(&self.log.events) {
            store.apply_event_at(at, event.clone()).expect("the beginning of a valid log is valid");
        }
        store
    }

    /// The store as it was at the time `t`: after all events logged at or
    /// before `t`. Like [`as_of`](Store::as_of), but by time instead of by
    /// position in the log.
    ///
    /// ```
    /// # use replayable::{model, Store, Timestamp};
    /// # use std::sync::atomic::{AtomicU64, Ordering};
    /// # model! { pub M { Note { text: String } } }
    /// # let ticks = AtomicU64::new(1_792_833_302_000);
    /// # let clock = move || Timestamp::from_unix_millis(ticks.fetch_add(60_000, Ordering::Relaxed));
    /// let mut store = Store::<M>::new().with_clock(clock); // a minute per event
    /// let note = store.create(Note { text: "draft".into() });
    /// store.edit(note).text = "final".into();
    ///
    /// let t: Timestamp = "2026-10-24T09:15:30.000Z".parse().unwrap();
    /// assert_eq!(store.as_of_time(t)[note].text, "draft");
    /// let t: Timestamp = "2026-10-24T09:10:00.000Z".parse().unwrap();
    /// assert!(!store.as_of_time(t).contains(note));
    /// ```
    pub fn as_of_time(&self, t: Timestamp) -> Self {
        self.as_of(self.log.times.partition_point(|&at| at <= t))
    }

    /// Applies one event, as if it had happened live, and adds it to the
    /// log with the current time. See [`apply_event_at`](Store::apply_event_at)
    /// to give the time yourself.
    ///
    /// Fails without changing anything if the event doesn't fit the current
    /// state.
    pub fn apply_event(&mut self, event: M::Event) -> Result<(), Error> {
        self.apply_event_at(self.log.now(), event)
    }

    /// Applies one event that happened at the time `at`, and adds it to the
    /// log. Used to follow a log that another store is writing, keeping its
    /// times.
    ///
    /// Fails without changing anything if the event doesn't fit the current
    /// state, or if `at` is earlier than the time of the last event.
    pub fn apply_event_at(&mut self, at: Timestamp, event: M::Event) -> Result<(), Error> {
        let index = self.log.events.len();
        if let Some(&last) = self.log.times.last() {
            if at < last {
                let reason = format!("its time {at} is before {last}, the time of the event before it");
                return Err(Error::InvalidEvent { index, reason });
            }
        }
        self.state.apply(&event).map_err(|reason| Error::InvalidEvent { index, reason })?;
        self.next_id = self.next_id.max(M::entity_id(&event).saturating_add(1));
        self.log.push(at, event);
        Ok(())
    }

    /// Builds up the state from a log (or the beginning of one).
    ///
    /// The events get the current time, as with
    /// [`apply_event`](Store::apply_event). To keep the times of a saved log,
    /// read it with [`read_json`](Store::read_json), or use
    /// [`as_of`](Store::as_of).
    pub fn replay(events: impl IntoIterator<Item = M::Event>) -> Result<Self, Error> {
        let mut store = Self::new();
        for event in events {
            store.apply_event(event)?;
        }
        Ok(store)
    }

    /// Writes the log as JSON Lines: one event per line, each ending in `\n`,
    /// with its time first: `{"at":"2026-10-24T09:15:02.123Z","CreateUser":[..]}`.
    ///
    /// Each event is written separately, so wrap files in a
    /// [`BufWriter`](std::io::BufWriter).
    pub fn write_json(&self, writer: impl Write) -> std::io::Result<()> {
        self.write_json_since(0, writer)
    }

    /// Like [`write_json`](Store::write_json), but only the events after the
    /// first `n`. Use it to append to a file that already holds the first
    /// `n` events.
    ///
    /// Panics if `n` is larger than the number of events.
    pub fn write_json_since(&self, n: usize, mut writer: impl Write) -> std::io::Result<()> {
        for (&at, event) in self.log.times[n..].iter().zip(&self.log.events[n..]) {
            codec::write_line::<M>(&mut writer, at, event)?;
            writer.write_all(b"\n")?;
        }
        Ok(())
    }

    /// The log as a JSON Lines string. See [`write_json`](Store::write_json).
    ///
    /// Fails only if a value can't be written as JSON, like a map with
    /// non-string keys inside one of your own value types.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        let mut out = Vec::new();
        for (&at, event) in self.log.times.iter().zip(&self.log.events) {
            codec::write_line::<M>(&mut out, at, event)?;
            out.push(b'\n');
        }
        Ok(String::from_utf8(out).expect("serde_json writes UTF-8"))
    }

    /// Replays a JSON Lines log, as written by [`write_json`](Store::write_json).
    /// Blank lines are skipped.
    ///
    /// The log may have been written by an older version of the model.
    /// Fields that didn't exist then get their default, old names declared
    /// with `(was ..)` are read as the new ones, and events about fields and
    /// entity types declared with `#![removed(..)]` are skipped (they aren't
    /// kept in [`events`](Store::events)). Any other name the model doesn't
    /// know is an [`Error::Json`].
    pub fn read_json(mut reader: impl BufRead) -> Result<Self, Error> {
        let mut store = Self::new();
        let mut line = String::new();
        let mut number = 0;
        while reader.read_line(&mut line)? > 0 {
            number += 1;
            store.apply_json_line(number, &line)?;
            line.clear();
        }
        Ok(store)
    }

    /// Like [`read_json`](Store::read_json), from a string.
    pub fn from_json(json: &str) -> Result<Self, Error> {
        let mut store = Self::new();
        for (i, line) in json.lines().enumerate() {
            store.apply_json_line(i + 1, line)?;
        }
        Ok(store)
    }

    fn apply_json_line(&mut self, number: usize, line: &str) -> Result<(), Error> {
        if line.trim().is_empty() {
            return Ok(());
        }
        let json_error = |source| Error::Json { line: number, source };
        let mut de = serde_json::Deserializer::from_str(line);
        let (at, parsed) = codec::parse_line::<M, _>(&mut de).map_err(json_error)?;
        de.end().map_err(json_error)?;
        self.apply_parsed(at, parsed)
    }

    fn apply_parsed(&mut self, at: Timestamp, parsed: Parsed<M::Event>) -> Result<(), Error> {
        match parsed {
            Parsed::Event(event) => self.apply_event_at(at, event),
            Parsed::Skipped { id } => {
                self.next_id = self.next_id.max(id.saturating_add(1));
                Ok(())
            }
        }
    }

    /// Writes the log in the compact binary format, usually a third of the
    /// size of the JSON and faster to read and write. The format is described
    /// in the [crate documentation](crate#binary-layout).
    ///
    /// Like JSON, it holds the names of fields and entity types, so a newer
    /// version of the model can read it the same way (see
    /// [`read_json`](Store::read_json)). Each name is written in full only
    /// once per file.
    pub fn write_binary(&self, writer: impl Write) -> std::io::Result<()> {
        self.write_binary_since(0, writer)
    }

    /// Like [`write_binary`](Store::write_binary), but only the events after
    /// the first `n`. Use it to append to a file that holds the first `n`
    /// events, as written or read by this store.
    ///
    /// The store remembers which names the file it last wrote to or read
    /// from has defined, so an append usually doesn't repeat them. If `n`
    /// doesn't match that file, the append starts by resetting the names
    /// instead, which is always safe. With `n == 0`, the start of a new file
    /// is written.
    ///
    /// ```
    /// # use replayable::{model, Store};
    /// # model! { pub World { User { name: String } } }
    /// let mut store = Store::<World>::new();
    /// let mut file = Vec::new(); // a File in real use
    /// let u = store.create(User { name: "a".into() });
    /// store.write_binary(&mut file)?;
    /// let saved = store.events().len();
    ///
    /// store.edit(u).name = "b".into();
    /// store.write_binary_since(saved, &mut file)?;
    ///
    /// let loaded = Store::<World>::from_binary(&file)?;
    /// assert_eq!(loaded[u].name, "b");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// Panics if `n` is larger than the number of events.
    pub fn write_binary_since(&self, n: usize, writer: impl Write) -> std::io::Result<()> {
        let mut state = self.binary.lock().unwrap_or_else(PoisonError::into_inner);
        binary::write::<M>(&mut state, n, &self.log.times, &self.log.events, writer)
    }

    /// The log in the binary format. See [`write_binary`](Store::write_binary).
    ///
    /// Fails only if one of your own value types fails to serialize.
    pub fn to_binary(&self) -> std::io::Result<Vec<u8>> {
        let mut out = Vec::new();
        // Not the store's own state, which is about the file it writes to.
        binary::write::<M>(&mut binary::WriteState::default(), 0, &self.log.times, &self.log.events, &mut out)?;
        Ok(out)
    }

    /// Replays a binary log, as written by [`write_binary`](Store::write_binary).
    /// An empty file is an empty log. Older versions of the model are handled
    /// as in [`read_json`](Store::read_json).
    ///
    /// The whole file is read into memory first.
    pub fn read_binary(mut reader: impl Read) -> Result<Self, Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        Self::from_binary(&bytes)
    }

    /// Like [`read_binary`](Store::read_binary), from bytes in memory.
    pub fn from_binary(bytes: &[u8]) -> Result<Self, Error> {
        let mut store = Self::new();
        let state = binary::read::<M>(bytes, |at, parsed| store.apply_parsed(at, parsed))?;
        let state = binary::appending(state, bytes, store.log.events.len());
        *store.binary.get_mut().unwrap_or_else(PoisonError::into_inner) = state;
        Ok(store)
    }

    /// Checks that the log really gives the current state: writes it as
    /// JSON and in the binary format, replays both, and compares every
    /// entity and the times. Returns a description of the first difference.
    ///
    /// Meant for tests. A difference means something changed without being
    /// logged, for example through a `Cell` inside a value, or that a value
    /// type doesn't survive a trip through JSON.
    pub fn verify(&self) -> Result<(), String> {
        let json = self.to_json().map_err(|e| format!("the log can't be written as JSON: {e}"))?;
        let from_json = Self::from_json(&json).map_err(|e| format!("the log can't be read back from JSON: {e}"))?;
        let bytes = self.to_binary().map_err(|e| format!("the log can't be written in the binary format: {e}"))?;
        let from_binary =
            Self::from_binary(&bytes).map_err(|e| format!("the log can't be read back from the binary format: {e}"))?;
        for (format, replayed) in [("JSON", from_json), ("the binary format", from_binary)] {
            if let Some(difference) = self.state.first_difference(&replayed.state) {
                return Err(format!("through {format}: {difference}"));
            }
            if replayed.log.times != self.log.times {
                return Err(format!("through {format}: the times of the events differ"));
            }
        }
        Ok(())
    }
}

/// `store[id]` works like reading from a `Vec` or `HashMap`.
impl<M: Contains<E>, E: Entity> Index<Id<E>> for Store<M> {
    type Output = E;
    fn index(&self, id: Id<E>) -> &E {
        match self.get(id) {
            Some(entity) => entity,
            None if self.editing == Some(id.raw()) => {
                panic!("{id:?} is being edited; use the value you're editing instead")
            }
            None => panic!("{id:?} doesn't exist"),
        }
    }
}

impl<M: Model> fmt::Debug for Store<M> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Store").field("events", &self.log.events.len()).field("next_id", &self.next_id).finish()
    }
}

/// A guard that works like `&mut E`. The changes become events when it's
/// dropped. Returned by [`Store::edit`].
///
/// Don't `mem::forget` it: the entity keeps the changes but no events are
/// logged, so a replay won't match.
pub struct Edit<'a, M: Contains<E>, E: Entity> {
    entity: &'a mut E,
    log: &'a mut Log<M>,
    id: Id<E>,
    before: Option<E::Snapshot>,
}

impl<M: Contains<E>, E: Entity> Edit<'_, M, E> {
    /// The id of the entity being edited.
    pub fn id(&self) -> Id<E> {
        self.id
    }
}

impl<M: Contains<E>, E: Entity> Deref for Edit<'_, M, E> {
    type Target = E;
    fn deref(&self) -> &E {
        self.entity
    }
}

impl<M: Contains<E>, E: Entity> DerefMut for Edit<'_, M, E> {
    fn deref_mut(&mut self) -> &mut E {
        self.entity
    }
}

impl<M: Contains<E>, E: Entity> Drop for Edit<'_, M, E> {
    fn drop(&mut self) {
        if let Some(before) = self.before.take() {
            let (id, log) = (self.id, &mut *self.log);
            self.entity.changes(before, &mut |c| log.push_live(M::updated(id, c)));
        }
    }
}
