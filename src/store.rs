use crate::{Error, Id};
use serde::{de::DeserializeOwned, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::io::{BufRead, Write};
use std::ops::{Deref, DerefMut, Index};
use std::panic::{self, AssertUnwindSafe};

/// An entity type declared in [`model!`](crate::model). Implemented by the
/// macro; you don't implement it yourself.
pub trait Entity: Default + fmt::Debug + 'static {
    /// The name of the type, used in `Debug` output of ids.
    const KIND: &'static str;
    /// A change to one field, generated as `<Entity>Change`.
    type Change: Clone + fmt::Debug + fmt::Display + Serialize + DeserializeOwned;
    /// What's needed to see what changed during an edit.
    type Snapshot;
    /// Taken before an edit.
    fn snapshot(&self) -> Self::Snapshot;
    /// Emits the changes made since `before` was taken.
    fn changes(&mut self, before: Self::Snapshot, emit: &mut dyn FnMut(Self::Change));
    /// Emits the changes that build the entity up from `Default`.
    fn initial(&mut self, emit: &mut dyn FnMut(Self::Change));
    /// Applies a logged change during replay.
    fn apply(&mut self, change: &Self::Change) -> Result<(), String>;
}

/// A whole model declared with [`model!`](crate::model): one table per
/// entity type, and the event type for all of them. Implemented by the
/// macro; you don't implement it yourself.
pub trait Model: Default {
    /// The event type, generated as `<Model>Event`.
    type Event: Clone + fmt::Debug + fmt::Display + Serialize + DeserializeOwned;
    /// Applies an event during replay. Fails if it doesn't fit the state.
    fn apply(&mut self, event: &Self::Event) -> Result<(), String>;
    /// The raw id of the entity the event is about.
    fn entity_id(event: &Self::Event) -> u64;
}

/// Says that the model `Self` has a table of `E`. Implemented by
/// [`model!`](crate::model) for every entity in the model, and used as the
/// bound on [`Store`] methods: `M: Contains<E>`.
pub trait Contains<E: Entity>: Model {
    /// The event for creating `id`.
    fn created(id: Id<E>) -> Self::Event;
    /// The event for a change to `id`.
    fn updated(id: Id<E>, change: E::Change) -> Self::Event;
    /// The event for deleting `id`.
    fn deleted(id: Id<E>) -> Self::Event;
    /// The table of `E`.
    fn table(&self) -> &BTreeMap<Id<E>, E>;
    /// The table of `E`, mutably.
    fn table_mut(&mut self) -> &mut BTreeMap<Id<E>, E>;
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
    log: Vec<M::Event>,
    next_id: u64,
    /// The raw id of the entity checked out by `update`, for a clear panic
    /// message if the closure tries to read it through the store.
    editing: Option<u64>,
}

impl<M: Model> Default for Store<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: Model> Store<M> {
    /// An empty store.
    pub fn new() -> Self {
        Store { state: M::default(), log: Vec::new(), next_id: 1, editing: None }
    }

    /// Creates an entity from a plain struct value and returns its id.
    pub fn create<E: Entity>(&mut self, mut value: E) -> Id<E>
    where
        M: Contains<E>,
    {
        let id = Id::from_raw(self.next_id);
        self.next_id += 1;
        let log = &mut self.log;
        log.push(M::created(id));
        value.initial(&mut |c| log.push(M::updated(id, c)));
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
        entity.changes(before, &mut |c| log.push(M::updated(id, c)));
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
        self.log.push(M::deleted(id));
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
        &self.log
    }

    /// The events after the first `n`. Save `events().len()` after writing
    /// the log, and pass it here next time to get only the new events.
    ///
    /// Panics if `n` is larger than the number of events.
    pub fn events_since(&self, n: usize) -> &[M::Event] {
        &self.log[n..]
    }

    /// Applies one event, as if it had happened live, and adds it to the
    /// log. Used to follow a log that another store is writing.
    ///
    /// Fails without changing anything if the event doesn't fit the current
    /// state.
    pub fn apply_event(&mut self, event: M::Event) -> Result<(), Error> {
        let index = self.log.len();
        self.state.apply(&event).map_err(|reason| Error::InvalidEvent { index, reason })?;
        self.next_id = self.next_id.max(M::entity_id(&event).saturating_add(1));
        self.log.push(event);
        Ok(())
    }

    /// Builds up the state from a log (or the beginning of one).
    pub fn replay(events: impl IntoIterator<Item = M::Event>) -> Result<Self, Error> {
        let mut store = Self::new();
        for event in events {
            store.apply_event(event)?;
        }
        Ok(store)
    }

    /// Writes the log as JSON Lines: one event per line, each ending in `\n`.
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
        for event in self.events_since(n) {
            serde_json::to_writer(&mut writer, event)?;
            writer.write_all(b"\n")?;
        }
        Ok(())
    }

    /// The log as a JSON Lines string. See [`write_json`](Store::write_json).
    pub fn to_json(&self) -> String {
        let mut out = Vec::new();
        self.write_json(&mut out).expect("writing to a Vec doesn't fail");
        String::from_utf8(out).expect("serde_json writes UTF-8")
    }

    /// Replays a JSON Lines log, as written by [`write_json`](Store::write_json).
    /// Blank lines are skipped.
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
        let event = serde_json::from_str(line).map_err(|source| Error::Json { line: number, source })?;
        self.apply_event(event)
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
        f.debug_struct("Store").field("events", &self.log.len()).field("next_id", &self.next_id).finish()
    }
}

/// A guard that works like `&mut E`. The changes become events when it's
/// dropped. Returned by [`Store::edit`].
///
/// Don't `mem::forget` it: the entity keeps the changes but no events are
/// logged, so a replay won't match.
pub struct Edit<'a, M: Contains<E>, E: Entity> {
    entity: &'a mut E,
    log: &'a mut Vec<M::Event>,
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
            self.entity.changes(before, &mut |c| log.push(M::updated(id, c)));
        }
    }
}
