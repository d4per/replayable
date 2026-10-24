//! Tests through the public API, from outside the crate, so they also check
//! that `model!` works when used from another crate.

use replayable::{model, Error, Id, List, ListOp, Map, Store, Value};
use serde::{Deserialize, Serialize};
use std::panic::{self, AssertUnwindSafe};

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

    let from_json = Store::<World>::from_json(&store.to_json()).unwrap();
    for s in [&replayed(&store), &from_json] {
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
fn create_logs_lists_as_one_event() {
    let mut store = Store::<World>::new();
    let u =
        store.create(User { name: "a".into(), tags: List::from(vec!["x".into(), "y".into()]), ..Default::default() });
    let events: Vec<String> = store.events().iter().map(|e| e.to_string()).collect();
    assert_eq!(events, ["create User id:1", r#"User id:1 set name="a""#, r#"User id:1 tags.splice(0..0, ["x", "y"])"#]);
    assert_eq!(replayed(&store)[u], store[u]);
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

    assert_eq!(store.delete(u).map(|u| u.name), Some("a".to_string()));
    assert!(store.delete(u).is_none());
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

    assert_eq!(String::from_utf8(file.clone()).unwrap(), store.to_json());
    let loaded = Store::<World>::read_json(file.as_slice()).unwrap();
    assert_eq!(loaded[u], store[u]);
}

#[test]
fn a_store_can_follow_another() {
    let mut leader = Store::<World>::new();
    let mut follower = Store::<World>::new();
    let u = leader.create(User::default());
    for i in 0..3 {
        leader.edit(u).age = i;
        leader.edit(u).tags.push(i.to_string());
        for e in leader.events_since(follower.events().len()) {
            follower.apply_event(e.clone()).unwrap();
        }
        assert_eq!(follower[u], leader[u]);
    }
}

#[test]
fn corrupt_logs_give_errors() {
    let mut store = Store::<World>::new();
    let u = store.create(User { tags: List::from(vec!["a".to_string()]), ..Default::default() });
    store.edit(u).tags.push("b".into());
    let json = store.to_json();
    let lines: Vec<&str> = json.lines().collect();

    // Not JSON at all, on line 2.
    let bad = format!("{}\nnot json\n", lines[0]);
    assert!(matches!(Store::<World>::from_json(&bad), Err(Error::Json { line: 2, .. })));
    assert!(matches!(Store::<World>::read_json(bad.as_bytes()), Err(Error::Json { line: 2, .. })));

    // An update before the create.
    let bad = lines[1..].join("\n");
    let err = Store::<World>::from_json(&bad).unwrap_err();
    assert!(matches!(err, Error::InvalidEvent { index: 0, .. }), "{err}");
    assert_eq!(err.to_string(), "event 0 can't be applied: User id:1 doesn't exist");

    // A create twice.
    let bad = format!("{}\n{}", lines[0], lines[0]);
    assert!(matches!(Store::<World>::from_json(&bad), Err(Error::InvalidEvent { index: 1, .. })));

    // An index out of range.
    let bad = json.replace(r#"{"Insert":[1,"b"]}"#, r#"{"Insert":[5,"b"]}"#);
    assert_ne!(bad, json);
    let err = Store::<World>::from_json(&bad).unwrap_err();
    assert_eq!(
        err.to_string(),
        "event 2 can't be applied: User id:1 tags: index 5 is out of range for a list of length 1"
    );

    // Removing a key that isn't there.
    let g = Id::<Group>::from_raw(1);
    let events = [
        WorldEvent::CreateGroup(g),
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
    let mut store = Store::<World>::new();
    let u = store.create(User { name: "a".into(), ..Default::default() });
    store.edit(u).tags.extend(["x".to_string(), "y".to_string()]);
    store.edit(u).tags.remove(0);
    store.edit(u).status = Status::Away("lunch".into());
    assert_eq!(
        store.to_json(),
        concat!(
            r#"{"CreateUser":1}"#,
            "\n",
            r#"{"UpdateUser":[1,{"Name":"a"}]}"#,
            "\n",
            r#"{"UpdateUser":[1,{"Tags":{"Splice":{"at":0,"remove":0,"insert":["x","y"]}}}]}"#,
            "\n",
            r#"{"UpdateUser":[1,{"Tags":{"Remove":0}}]}"#,
            "\n",
            r#"{"UpdateUser":[1,{"Status":{"Away":"lunch"}}]}"#,
            "\n",
        )
    );
}
