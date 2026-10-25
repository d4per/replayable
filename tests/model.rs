//! Tests through the public API, from outside the crate, so they also check
//! that `model!` works when used from another crate.

use replayable::{model, Error, Id, List, ListOp, Map, Position, Store, Timestamp, Value};
use serde::{Deserialize, Serialize};
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
enum Status {
    #[default]
    Active,
    Away(String),
}
impl Value for Status {}

model! {
    /// The test model.
    pub World {
        /// A user.
        User {
            /// The name.
            name: String,
            age: u8,
            parent: User,
            tags: List<String>,
            status: Status,
            nick: Option<String>,
        }
        Group {
            title: String,
            members: List<User>,
            roles: Map<User, String>,
        }
    }
}

// A second model with private items, in its own module.
mod private {
    use replayable::{model, List, Map};

    model! {
        Notes {
            Note { text: String, lines: List<String>, next: Note, links: List<Note>, by: Map<Note, Note> }
        }
    }

    #[test]
    fn private_model_works() {
        let mut store = replayable::Store::<Notes>::new();
        let a = store.create(Note { text: "a".into(), ..Default::default() });
        let b = store.create(Note { next: Some(a), ..Default::default() });
        store.edit(b).lines.push("x".into());
        store.edit(b).links.push(a);
        store.edit(a).by.insert(b, a);
        let replayed = replayable::Store::<Notes>::replay(store.events().to_vec()).unwrap();
        assert_eq!(replayed[b], store[b]);
    }
}

/// A clock that starts at 2026-10-24T09:15:02.123Z and moves `step`
/// milliseconds each time it's read.
fn ticking(step: u64) -> impl Fn() -> Timestamp + Send + Sync {
    let next = AtomicU64::new(1_792_833_302_123);
    move || Timestamp::from_unix_millis(next.fetch_add(step, Ordering::Relaxed))
}

fn replayed(store: &Store<World>) -> Store<World> {
    Store::replay(store.events().iter().cloned()).unwrap()
}

struct Rng(u64);
impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) % n.max(1) as u64) as usize
    }
}

/// Makes thousands of random changes to live data and checks that a replay
/// of the log (also through JSON) gives exactly the same state.
#[test]
fn replay_matches_live_state() {
    let mut rng = Rng(777);
    let mut store = Store::<World>::new();
    let users: Vec<Id<User>> = (0..5).map(|i| store.create(User { age: i * 10, ..Default::default() })).collect();
    let group = store.create(Group { members: List::from(vec![users[0], users[1]]), ..Default::default() });

    for step in 0..5000 {
        let u = users[rng.below(users.len())];
        let pick = users[rng.below(users.len())];
        match rng.below(30) {
            0 => store.edit(u).age = rng.below(100) as u8,
            1 => store.edit(u).tags.push(format!("t{}", rng.below(5))),
            2 => {
                let mut user = store.edit(u);
                if !user.tags.is_empty() {
                    let i = rng.below(user.tags.len());
                    user.tags[i] = format!("x{step}");
                }
            }
            3 => store.edit(u).tags.sort(),
            4 => store.edit(u).tags.retain(|t| !t.ends_with('1')),
            5 => store.edit(group).members.push(pick),
            6 => {
                let mut g = store.edit(group);
                let i = rng.below(g.members.len() + 1);
                g.members.insert(i, pick);
            }
            7 => {
                let mut g = store.edit(group);
                if !g.members.is_empty() {
                    let i = rng.below(g.members.len());
                    g.members.remove(i);
                }
            }
            8 => {
                let mut g = store.edit(group);
                if !g.members.is_empty() {
                    let i = rng.below(g.members.len());
                    g.members.set(i, pick);
                }
            }
            9 => store.update(group, |g, s| g.members.retain(|&m| s[m].age % 3 != 0)),
            10 => {
                let mut g = store.edit(group);
                g.members.pop();
                g.members.extend([pick, pick]);
            }
            11 => store.edit(u).tags.iter_mut().for_each(|t| t.make_ascii_uppercase()),
            12 => {
                let mut user = store.edit(u);
                let n = user.tags.len();
                if n > 0 {
                    user.tags.swap(rng.below(n), rng.below(n));
                }
            }
            13 => {
                let mut g = store.edit(group);
                let i = rng.below(g.members.len() + 1);
                if let Some(m) = g.members.get_mut(i) {
                    *m = pick;
                }
            }
            14 => store.edit(group).members.dedup(),
            // The whole list is replaced
            15 => store.edit(group).members = List::from(vec![pick]),
            // The same list out and back in, changed on the way
            16 => {
                let mut g = store.edit(group);
                let mut members = std::mem::take(&mut g.members);
                members.push(pick);
                g.members = members;
            }
            // The whole entity is replaced
            17 => {
                *store.edit(u) = User {
                    age: rng.below(100) as u8,
                    tags: List::from(vec![format!("new{step}")]),
                    ..Default::default()
                }
            }
            18 => {
                store.edit(group).roles.insert(pick, format!("r{}", rng.below(3)));
            }
            19 => {
                store.edit(group).roles.remove(&pick);
            }
            20 => {
                let mut g = store.edit(group);
                if let Some(role) = g.roles.get_mut(&pick) {
                    role.push('+');
                }
            }
            21 => store.update(group, |g, s| g.roles.retain(|&m, _| s[m].age % 2 == 0)),
            22 => store.edit(group).roles.entry(pick).or_default().push('e'),
            23 => {
                let n = rng.below(4);
                store.edit(u).tags.extend((0..n).map(|i| format!("e{i}")));
            }
            24 => {
                let mut user = store.edit(u);
                let len = rng.below(user.tags.len() + 1);
                user.tags.truncate(len);
            }
            25 => {
                let mut user = store.edit(u);
                let n = user.tags.len();
                let (a, b) = (rng.below(n + 1), rng.below(n + 1));
                user.tags.drain(a.min(b)..a.max(b));
            }
            26 => {
                // Several kinds of change in one edit, interleaved.
                let mut user = store.edit(u);
                user.tags.push("p".into());
                user.tags.sort();
                user.tags.insert(0, "i".into());
                if user.tags.len() > 2 {
                    user.tags[1] = "s".into();
                }
                user.tags.retain(|t| t != "e0");
            }
            27 => {
                store.edit(u).status = if rng.below(2) == 0 { Status::Active } else { Status::Away(format!("a{step}")) }
            }
            28 => store.edit(u).nick = Some(format!("n{}", rng.below(3))).filter(|_| rng.below(3) > 0),
            _ => match rng.below(60) {
                0 => store.edit(group).members.clear(),
                1 => store.edit(group).roles.clear(),
                2 => store.edit(group).roles = Map::from_iter([(pick, "new".to_string())]),
                _ => {}
            },
        }
    }

    let from_json = Store::<World>::from_json(&store.to_json().unwrap()).unwrap();
    let from_binary = Store::<World>::from_binary(&store.to_binary().unwrap()).unwrap();
    for s in [&replayed(&store), &from_json, &from_binary] {
        for &u in &users {
            assert_eq!(s[u], store[u]);
        }
        assert_eq!(s[group], store[group]);
    }
}

/// Replacing a whole collection is noticed and logged.
#[test]
fn replacing_a_whole_collection_is_logged() {
    let mut store = Store::<World>::new();
    let a = store.create(User::default());
    let b = store.create(User::default());
    let g = store.create(Group { members: List::from(vec![a, b]), ..Default::default() });

    store.edit(g).members = List::from(vec![b]);
    assert_eq!(replayed(&store)[g], store[g]);

    *store.edit(g) = Group { title: "new".into(), members: List::from(vec![a]), ..Default::default() };
    assert_eq!(replayed(&store)[g], store[g]);

    // Changes made to another list must not be logged as if they were made
    // to the one that was in the field.
    let mut other = List::from(vec![b, b]);
    other.push(a);
    store.edit(g).members = other;
    assert_eq!(replayed(&store)[g], store[g]);

    store.edit(g).roles = Map::from_iter([(a, "x".to_string())]);
    assert_eq!(replayed(&store)[g], store[g]);
    assert_eq!(store[g].members.as_slice(), &[b, b, a]);
}

/// Tracked changes give exactly the events you'd expect, nothing more.
#[test]
fn tracked_edits_give_minimal_events() {
    let mut store = Store::<World>::new();
    let u = store.create(User { tags: List::from(vec!["a".into(), "b".into()]), ..Default::default() });
    let g = store.create(Group::default());
    let n = store.events().len();

    store.edit(u).tags[1] = "c".into();
    let _ = store.edit(u).tags.get_mut(0); // borrowed but not changed
    store.edit(u).tags.sort(); // already sorted
    {
        // The same list out and back in gives just the change itself.
        let mut user = store.edit(u);
        let mut tags = std::mem::take(&mut user.tags);
        tags.push("d".into());
        user.tags = tags;
    }
    store.edit(u).tags.extend(["e".to_string(), "f".to_string()]);
    store.edit(u).tags.truncate(1);
    store.edit(g).roles.insert(u, "x".into());
    store.edit(g).roles.entry(u).or_default().push('y');

    let new: Vec<String> = store.events_since(n).iter().map(|e| e.to_string()).collect();
    assert_eq!(
        new,
        [
            r#"User id:1 tags[1] = "c""#,
            r#"User id:1 tags.insert(2, "d")"#,
            r#"User id:1 tags.splice(3..3, ["e", "f"])"#,
            r#"User id:1 tags.splice(1..5, [])"#,
            r#"Group id:2 roles.insert(User id:1, "x")"#,
            r#"Group id:2 roles.insert(User id:1, "xy")"#,
        ]
    );
}

#[test]
fn create_logs_the_whole_value_as_one_event() {
    let mut store = Store::<World>::new();
    let u =
        store.create(User { name: "a".into(), tags: List::from(vec!["x".into(), "y".into()]), ..Default::default() });
    let events: Vec<String> = store.events().iter().map(|e| e.to_string()).collect();
    assert_eq!(
        events,
        [r#"create User id:1 { name: "a", age: 0, parent: None, tags: ["x", "y"], status: Active, nick: None }"#]
    );
    assert_eq!(replayed(&store)[u], store[u]);

    // Changes made to a list before `create` aren't logged a second time.
    let mut tags = List::new();
    tags.push("p".to_string());
    let v = store.create(User { tags, ..Default::default() });
    store.edit(v).tags.push("q".into());
    assert_eq!(replayed(&store)[v], store[v]);
    assert_eq!(store[v].tags.as_slice(), ["p", "q"]);
}

#[test]
#[should_panic(expected = "is being edited")]
fn reading_the_entity_being_edited_panics_clearly() {
    let mut store = Store::<World>::new();
    let u = store.create(User::default());
    store.update(u, |_, s| s[u].age);
}

#[test]
fn panic_inside_update_puts_entity_back() {
    let mut store = Store::<World>::new();
    let u = store.create(User::default());
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        store.update(u, |user, _| {
            user.age = 7;
            panic!("oops");
        })
    }));
    assert!(result.is_err());
    assert_eq!(store[u].age, 7);
    assert_eq!(replayed(&store)[u], store[u]);
}

#[test]
fn forgetting_an_edit_keeps_the_entity() {
    let mut store = Store::<World>::new();
    let u = store.create(User::default());
    std::mem::forget(store.edit(u));
    assert!(store.contains(u));
}

#[test]
fn fallible_access() {
    let mut store = Store::<World>::new();
    let u = store.create(User { name: "a".into(), ..Default::default() });
    let g = store.create(Group::default());
    let missing = Id::<User>::from_raw(99);

    assert!(store.try_edit(missing).is_none());
    assert_eq!(store.try_update(missing, |u, _| u.age), None);
    assert_eq!(store.try_update(u, |u, _| u.name.clone()), Some("a".to_string()));
    assert!(store.get(missing).is_none());
    assert_eq!((store.count::<User>(), store.count::<Group>()), (1, 1));
    assert_eq!(store.try_edit(u).map(|e| e.id()), Some(u));

    store.pin(g);
    assert_eq!(store.collect(), 1);
    assert_eq!(store.collect(), 0);
    assert!(!store.contains(u));
    assert_eq!(store.all::<User>().count(), 0);
    assert_eq!(store.all::<Group>().map(|(id, _)| id).collect::<Vec<_>>(), [g]);

    // Ids keep counting after a replay, also when the last entity was deleted.
    let mut replayed = replayed(&store);
    let next = replayed.create(User::default());
    assert_eq!(next.raw(), 3);
}

#[test]
fn incremental_json_matches_full_json() {
    let mut store = Store::<World>::new();
    let u = store.create(User::default());
    let mut file = Vec::new();
    store.write_json(&mut file).unwrap();
    let mut saved = store.events().len();

    for i in 0..3 {
        store.edit(u).tags.push(format!("t{i}"));
        store.write_json_since(saved, &mut file).unwrap();
        saved = store.events().len();
    }

    assert_eq!(String::from_utf8(file.clone()).unwrap(), store.to_json().unwrap());
    let loaded = Store::<World>::read_json(file.as_slice()).unwrap();
    assert_eq!(loaded[u], store[u]);
}

#[test]
fn deleted_entities_can_be_seen_in_earlier_states() {
    let mut store = Store::<World>::new();
    let u = store.create(User { name: "a".into(), ..Default::default() });
    let g = store.create(Group { members: List::from(vec![u]), ..Default::default() });
    store.pin(g);
    let before_delete = store.events().len();
    store.edit(g).members.clear();
    store.collect();
    assert!(!store.contains(u));

    // The log still holds both the create and the delete.
    let log: Vec<String> = store.events().iter().map(|e| e.to_string()).collect();
    assert!(log[0].starts_with("create User id:1"));
    assert_eq!(log.last().unwrap(), "delete User id:1");

    let then = store.as_of(before_delete);
    assert_eq!(then[u].name, "a");
    assert_eq!(then[g].members.as_slice(), [u]);
    assert!(!store.as_of(0).contains(u));
    assert_eq!(store.as_of(store.events().len()).count::<User>(), 0);
}

#[test]
fn events_are_logged_with_their_time() {
    let mut store = Store::<World>::new().with_clock(ticking(1000));
    let u = store.create(User::default());
    store.edit(u).age = 1;
    store.edit(u).tags.push("a".into());
    let times: Vec<String> = store.timestamps().iter().map(|t| t.to_string()).collect();
    assert_eq!(times, ["2026-10-24T09:15:02.123Z", "2026-10-24T09:15:03.123Z", "2026-10-24T09:15:04.123Z"]);

    // The times are kept by a trip through JSON and by `as_of`.
    let loaded = Store::<World>::from_json(&store.to_json().unwrap()).unwrap();
    assert_eq!(loaded.timestamps(), store.timestamps());
    assert_eq!(store.as_of(2).timestamps(), &store.timestamps()[..2]);

    // `as_of_time` includes events at exactly that time.
    let t = store.timestamps()[1];
    assert_eq!(store.as_of_time(t)[u].age, 1);
    assert!(store.as_of_time(t)[u].tags.is_empty());
    assert_eq!(store.as_of_time(Timestamp::UNIX_EPOCH).count::<User>(), 0);
    assert_eq!(store.as_of_time("9999-01-01T00:00:00.000Z".parse().unwrap())[u], store[u]);
}

#[test]
fn times_never_go_backwards() {
    // A clock that is set back an hour after the first event.
    let reads = AtomicU64::new(0);
    let clock = move || {
        let back = if reads.fetch_add(1, Ordering::Relaxed) == 0 { 0 } else { 3_600_000 };
        Timestamp::from_unix_millis(1_792_833_302_123 - back)
    };
    let mut store = Store::<World>::new().with_clock(clock);
    let u = store.create(User::default());
    store.edit(u).age = 1;
    assert_eq!(store.timestamps()[0], store.timestamps()[1]);

    // An event from the past can't be added to the log...
    let before = store.events().len();
    let event = WorldEvent::UpdateUser(u, UserChange::Age(2));
    let err = store.apply_event_at(Timestamp::UNIX_EPOCH, event).unwrap_err();
    assert!(matches!(err, Error::InvalidEvent { index: 2, .. }), "{err}");
    assert!(err.to_string().contains("is before 2026-10-24T09:15:02.123Z"), "{err}");
    assert_eq!((store.events().len(), store[u].age), (before, 1));

    // ...and a log whose times go backwards can't be read.
    let json = store.to_json().unwrap();
    let bad = json.replacen("2026-10-24T09:15:02.123Z", "2026-10-24T09:15:02.124Z", 1);
    assert!(matches!(Store::<World>::from_json(&bad), Err(Error::InvalidEvent { index: 1, .. })));
}

#[test]
fn lines_without_a_valid_time_are_errors() {
    let event = r#""CreateUser":[1,{"name":"x"}]"#;
    for (line, message) in [
        (format!("{{{event}}}"), "missing field `at`"),
        (format!(r#"{{"at":"2026-10-24 09:15:02",{event}}}"#), "isn't a time like 2026-10-24T09:15:02.123Z"),
        (format!(r#"{{"at":"2026-10-24T09:15:02.123Z","at":"2026-10-24T09:15:02.123Z",{event}}}"#), "duplicate"),
        (r#"{"at":"2026-10-24T09:15:02.123Z"}"#.to_string(), "invalid length"),
        (format!(r#"{{"at":"2026-10-24T09:15:02.123Z",{event},"DeleteUser":1}}"#), "a second event"),
    ] {
        let err = Store::<World>::from_json(&line).unwrap_err();
        assert!(matches!(err, Error::Json { line: 1, .. }), "{err}");
        assert!(err.to_string().contains(message), "{line}: {err}");
    }
    // The time may come after the event too.
    let line = format!(r#"{{{event},"at":"2026-10-24T09:15:02.123Z"}}"#);
    assert_eq!(Store::<World>::from_json(&line).unwrap().timestamps()[0].unix_millis(), 1_792_833_302_123);
}

#[test]
fn a_store_can_follow_another() {
    let mut leader = Store::<World>::new();
    let mut follower = Store::<World>::new();
    let u = leader.create(User::default());
    for i in 0..3 {
        leader.edit(u).age = i;
        leader.edit(u).tags.push(i.to_string());
        let n = follower.events().len();
        for (&at, e) in leader.timestamps()[n..].iter().zip(leader.events_since(n)) {
            follower.apply_event_at(at, e.clone()).unwrap();
        }
        assert_eq!(follower[u], leader[u]);
        assert_eq!(follower.timestamps(), leader.timestamps());
    }
}

#[test]
fn corrupt_logs_give_errors() {
    let mut store = Store::<World>::new();
    let u = store.create(User { tags: List::from(vec!["a".to_string()]), ..Default::default() });
    store.edit(u).tags.push("b".into());
    let json = store.to_json().unwrap();
    let lines: Vec<&str> = json.lines().collect();

    // Not JSON at all, on line 2.
    let bad = format!("{}\nnot json\n", lines[0]);
    assert!(matches!(Store::<World>::from_json(&bad), Err(Error::Json { line: 2, .. })));
    assert!(matches!(Store::<World>::read_json(bad.as_bytes()), Err(Error::Json { line: 2, .. })));

    // An update before the create.
    let bad = lines[1..].join("\n");
    let err = Store::<World>::from_json(&bad).unwrap_err();
    assert!(matches!(err, Error::InvalidEvent { index: 0, at: Some(Position::Line(1)), .. }), "{err}");
    assert_eq!(err.to_string(), "event 0, at line 1, can't be applied: User id:1 doesn't exist");

    // Blank lines count as lines, but not as events.
    let bad = format!("{}\n\n{}", lines[0], lines[0]);
    let err = Store::<World>::from_json(&bad).unwrap_err();
    assert!(matches!(err, Error::InvalidEvent { index: 1, at: Some(Position::Line(3)), .. }), "{err}");

    // In the binary format, the byte offset of the event.
    let mut twice = Store::<World>::new();
    twice.apply_event(store.events()[0].clone()).unwrap();
    let mut bytes = twice.to_binary().unwrap();
    bytes.push(0); // a reset, so that the event can be copied after it, header left out
    let at = bytes.len() as u64;
    bytes.extend_from_slice(&twice.to_binary().unwrap()[5..]);
    let err = Store::<World>::from_binary(&bytes).unwrap_err();
    assert!(matches!(err, Error::InvalidEvent { index: 1, at: Some(Position::Offset(o)), .. } if o == at), "{err}");

    // A create twice.
    let bad = format!("{}\n{}", lines[0], lines[0]);
    assert!(matches!(Store::<World>::from_json(&bad), Err(Error::InvalidEvent { index: 1, .. })));

    // An index out of range.
    let bad = json.replace(r#"{"Insert":[1,"b"]}"#, r#"{"Insert":[5,"b"]}"#);
    assert_ne!(bad, json);
    let err = Store::<World>::from_json(&bad).unwrap_err();
    assert_eq!(
        err.to_string(),
        "event 1, at line 2, can't be applied: User id:1 tags: index 5 is out of range for a list of length 1"
    );

    // Removing a key that isn't there.
    let g = Id::<Group>::from_raw(1);
    let events = [
        WorldEvent::CreateGroup(g, Box::default()),
        WorldEvent::UpdateGroup(g, GroupChange::Roles(replayable::MapOp::Remove(Id::from_raw(1)))),
    ];
    assert!(Store::<World>::replay(events).is_err());

    // A failed event leaves the store as it was.
    let mut store = Store::<World>::new();
    let u = store.create(User::default());
    let before = store.events().len();
    assert!(store.apply_event(WorldEvent::UpdateUser(u, UserChange::Tags(ListOp::Remove(0)))).is_err());
    assert_eq!(store.events().len(), before);
}

#[test]
fn json_format_is_stable() {
    let mut store = Store::<World>::new().with_clock(ticking(1));
    let u = store.create(User { name: "a".into(), ..Default::default() });
    let g = store.create(Group { roles: Map::from_iter([(u, "x".to_string())]), ..Default::default() });
    store.edit(u).tags.extend(["x".to_string(), "y".to_string()]);
    store.edit(u).tags.remove(0);
    store.edit(u).status = Status::Away("lunch".into());
    store.edit(g).members.push(u);
    store.pin(u);
    store.pin(g);
    store.unpin(g);
    store.collect();
    assert_eq!(
        store.to_json().unwrap(),
        concat!(
            r#"{"at":"2026-10-24T09:15:02.123Z","CreateUser":[1,{"name":"a","age":0,"parent":null,"tags":[],"status":"Active","nick":null}]}"#,
            "\n",
            r#"{"at":"2026-10-24T09:15:02.124Z","CreateGroup":[2,{"title":"","members":[],"roles":[[1,"x"]]}]}"#,
            "\n",
            r#"{"at":"2026-10-24T09:15:02.125Z","UpdateUser":[1,{"tags":{"Splice":{"at":0,"remove":0,"insert":["x","y"]}}}]}"#,
            "\n",
            r#"{"at":"2026-10-24T09:15:02.126Z","UpdateUser":[1,{"tags":{"Remove":0}}]}"#,
            "\n",
            r#"{"at":"2026-10-24T09:15:02.127Z","UpdateUser":[1,{"status":{"Away":"lunch"}}]}"#,
            "\n",
            r#"{"at":"2026-10-24T09:15:02.128Z","UpdateGroup":[2,{"members":{"Insert":[0,1]}}]}"#,
            "\n",
            r#"{"at":"2026-10-24T09:15:02.129Z","PinUser":1}"#,
            "\n",
            r#"{"at":"2026-10-24T09:15:02.130Z","PinGroup":2}"#,
            "\n",
            r#"{"at":"2026-10-24T09:15:02.131Z","UnpinGroup":2}"#,
            "\n",
            r#"{"at":"2026-10-24T09:15:02.132Z","DeleteGroup":2}"#,
            "\n",
        )
    );
}

#[test]
#[should_panic(expected = "NaN can't be stored")]
fn nan_in_a_field_is_rejected() {
    replayable::model! { Floats { Point { x: f64, xs: List<f64> } } }
    let mut store = Store::<Floats>::new();
    let p = store.create(Point::default());
    store.edit(p).x = f64::NAN;
}

#[test]
#[should_panic(expected = "inf can't be stored")]
fn infinity_in_a_list_is_rejected() {
    replayable::model! { Floats { Point { x: f64, xs: List<f64> } } }
    let mut store = Store::<Floats>::new();
    let p = store.create(Point::default());
    store.edit(p).xs.push(f64::INFINITY);
}

#[test]
fn a_rejected_value_is_undone_and_the_rest_is_logged() {
    replayable::model! { Floats { Point { name: String, x: f64, xs: List<f64>, by: Map<u8, f64> } } }
    let mut store = Store::<Floats>::new();
    let p = store.create(Point { x: 1.0, xs: vec![1.0, 2.0].into(), ..Default::default() });
    let rejected = |store: &mut Store<Floats>, f: &dyn Fn(&mut Point)| {
        let result = panic::catch_unwind(AssertUnwindSafe(|| f(&mut store.edit(p))));
        let message = *result.unwrap_err().downcast::<String>().unwrap();
        assert_eq!(store.verify(), Ok(()), "after {message}");
        message
    };

    let message = rejected(&mut store, &|p| {
        p.name = "a".into();
        p.x = f64::NAN;
        p.xs.push(3.0);
    });
    assert!(message.contains("x: NaN can't be stored"), "{message}");
    assert_eq!(store[p].x, 1.0);
    assert_eq!((store[p].name.as_str(), store[p].xs.as_slice()), ("a", &[1.0, 2.0, 3.0][..]));

    rejected(&mut store, &|p| {
        p.xs.push(f64::NAN);
        p.xs[0] = f64::INFINITY;
        p.xs.push(4.0);
    });
    assert_eq!(store[p].xs.as_slice(), [2.0, 3.0, 4.0]);
    rejected(&mut store, &|p| p.xs = vec![5.0, f64::NAN].into());
    assert_eq!(store[p].xs.as_slice(), [5.0]);
    // Through DerefMut, found by the diff.
    rejected(&mut store, &|p| p.xs.as_mut_slice()[0] = f64::NAN);
    assert!(store[p].xs.is_empty());
    rejected(&mut store, &|p| {
        p.by.insert(1, 1.0);
        p.by.insert(2, f64::NAN);
    });
    assert_eq!(store[p].by.iter().collect::<Vec<_>>(), [(&1, &1.0)]);
}

#[test]
fn a_rejected_value_in_update_keeps_the_entity() {
    replayable::model! { Floats { Point { x: f64 } } }
    let mut store = Store::<Floats>::new();
    let p = store.create(Point::default());
    let result = panic::catch_unwind(AssertUnwindSafe(|| store.update(p, |p, _| p.x = f64::NAN)));
    assert!(result.is_err());
    assert_eq!(store[p].x, 0.0);
    assert_eq!(store.verify(), Ok(()));
}

#[test]
fn tuples_and_arrays_are_values() {
    replayable::model! { Shapes { Shape { at: (f64, f64), corners: List<[u8; 2]>, by: Map<(u8, u8), String> } } }
    let mut store = Store::<Shapes>::new();
    let s = store.create(Shape { at: (1.0, -2.5), ..Default::default() });
    store.edit(s).corners.push([1, 2]);
    store.edit(s).by.insert((0, 1), "a".into());
    assert_eq!(store.verify(), Ok(()));
    let result = panic::catch_unwind(AssertUnwindSafe(|| store.edit(s).at.1 = f64::NAN));
    assert!(result.is_err());
    assert_eq!(store[s].at, (1.0, -2.5));
}

#[test]
fn pins_are_checked_against_the_type() {
    let mut store = Store::<World>::new();
    let u = store.create(User::default());
    store.pin(u);
    let as_group = Id::<Group>::from_raw(u.raw());
    assert!(!store.is_pinned(as_group));
    assert!(!store.unpin(as_group));
    assert!(store.is_pinned(u));
    assert_eq!(store.verify(), Ok(()));
}

#[test]
#[should_panic(expected = "Some(None) can't be stored")]
fn some_none_is_rejected() {
    replayable::model! { Nested { Thing { maybe: Option<Option<u8>> } } }
    let mut store = Store::<Nested>::new();
    let t = store.create(Thing::default());
    store.edit(t).maybe = Some(Some(1));
    store.edit(t).maybe = Some(None);
}

#[test]
#[should_panic(expected = "can't log")]
fn nan_at_create_is_rejected() {
    replayable::model! { Floats { Point { x: f64 } } }
    let mut store = Store::<Floats>::new();
    store.create(Point { x: f64::NAN });
}

/// A value type whose serde loses data. Debug builds catch it as soon as it's
/// logged; `verify` catches it in any build.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
struct Lossy {
    kept: u32,
    #[serde(skip)]
    lost: u32,
}
impl Value for Lossy {}

#[test]
#[cfg_attr(debug_assertions, should_panic(expected = "changes on a trip through JSON"))]
fn lossy_value_types_are_caught() {
    replayable::model! { Lossless { Thing { value: Lossy } } }
    let mut store = Store::<Lossless>::new();
    let t = store.create(Thing::default());
    store.edit(t).value.lost = 1;
    assert!(store.verify().is_err());
}

/// State changed without going through an edit is found by `verify`.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
struct Counter(std::cell::Cell<u32>);
impl Value for Counter {}

#[test]
fn verify_finds_changes_that_were_not_logged() {
    replayable::model! { Counters { Thing { count: Counter } } }
    let mut store = Store::<Counters>::new();
    let t = store.create(Thing::default());
    store.edit(t).count = Counter(std::cell::Cell::new(1));
    assert_eq!(store.verify(), Ok(()));

    store[t].count.0.set(2); // through a shared reference, so not logged
    let err = store.verify().unwrap_err();
    assert!(err.contains("Thing id:1 differs after a replay"), "{err}");
}

#[test]
fn verify_passes_after_many_random_changes() {
    let mut store = Store::<World>::new();
    let users: Vec<Id<User>> = (0..3).map(|_| store.create(User::default())).collect();
    let g = store.create(Group::default());
    for (i, &u) in users.iter().enumerate() {
        store.edit(u).tags.extend((0..i).map(|j| j.to_string()));
        store.edit(g).members.push(u);
        store.edit(g).roles.insert(u, i.to_string());
    }
    store.pin(g);
    store.edit(g).members.remove(0);
    store.edit(g).roles.remove(&users[0]);
    assert_eq!(store.collect(), 1);
    assert_eq!(store.verify(), Ok(()));
}

#[test]
fn entities_without_fields_work() {
    replayable::model! { Empty { Marker {} } }
    let mut store = Store::<Empty>::new();
    let m = store.create(Marker {});
    let _ = store.edit(m);
    assert_eq!(store.events().len(), 1);
    assert_eq!(store.events()[0].to_string(), "create Marker id:1 { }");
    assert_eq!(store.verify(), Ok(()));
}
