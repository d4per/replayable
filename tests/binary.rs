//! The binary log format, through the public API.

use replayable::{model, Error, List, Map, Store, Timestamp, Value};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
enum Mood {
    #[default]
    Calm,
    Busy(u32),
    Away {
        until: Timestamp,
        note: Option<String>,
    },
}
impl Value for Mood {}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
struct Point {
    x: f64,
    y: f64,
}
impl Value for Point {}

model! {
    pub World {
        User {
            name: String,
            age: u8,
            score: i64,
            mood: Mood,
            seen: Timestamp,
            home: Option<Point>,
            friends: List<User>,
            notes: Map<String, String>,
            ratings: Map<User, i8>,
        }
        Topic { title: String, posts: List<String> }
    }
}

/// A clock that starts at 2026-10-24T09:15:02.123Z and moves by the given
/// steps in turn, in milliseconds.
fn clock(steps: &'static [u64]) -> impl Fn() -> Timestamp + Send + Sync {
    let ticks = AtomicU64::new(0);
    move || {
        let i = ticks.fetch_add(1, Ordering::Relaxed) as usize;
        let elapsed: u64 = steps.iter().cycle().take(i).sum();
        Timestamp::from_unix_millis(1_792_833_302_123 + elapsed)
    }
}

/// A store with a bit of everything, and times that are sometimes the same
/// and sometimes days apart.
fn sample() -> Store<World> {
    let mut store = Store::<World>::new().with_clock(clock(&[0, 1, 0, 0, 5_000, 86_400_000 * 3]));
    let a = store.create(User { name: "Ann".into(), age: 30, score: -5, ..Default::default() });
    let b = store.create(User { name: "Bo".into(), home: Some(Point { x: 1.5, y: -2.0 }), ..Default::default() });
    for i in 0..20 {
        let mut user = store.edit(if i % 2 == 0 { a } else { b });
        user.score -= i * 1000;
        user.friends.push(if i % 2 == 0 { b } else { a });
        user.notes.insert(format!("note {}", i % 3), "x".repeat(i as usize));
    }
    store.edit(a).mood = Mood::Busy(3);
    store.edit(b).mood = Mood::Away { until: "2027-01-01T00:00:00.000Z".parse().unwrap(), note: Some("ski".into()) };
    store.edit(a).seen = Timestamp::from_unix_millis(1);
    store.edit(a).ratings.insert(b, -3);
    store.edit(a).friends.retain(|&f| f != b);
    let t = store.create(Topic { title: "hello".into(), ..Default::default() });
    store.edit(t).posts.extend(["one".to_string(), "two".to_string()]);
    store.delete(t);
    store
}

#[track_caller]
fn assert_same(a: &Store<World>, b: &Store<World>) {
    assert_eq!(a.events(), b.events());
    assert_eq!(a.timestamps(), b.timestamps());
    for (id, user) in a.all::<User>() {
        assert_eq!(&b[id], user);
    }
    assert_eq!(a.count::<User>(), b.count::<User>());
}

#[test]
fn a_log_reads_back_the_same() {
    let store = sample();
    let bytes = store.to_binary().unwrap();
    assert!(bytes.starts_with(b"RPLB\x01"));
    assert_same(&store, &Store::from_binary(&bytes).unwrap());
    assert_same(&store, &Store::read_binary(bytes.as_slice()).unwrap());
    assert_eq!(store.verify(), Ok(()));
}

#[test]
fn it_converts_to_and_from_json() {
    let store = sample();
    let from_json = Store::<World>::from_json(&store.to_json().unwrap()).unwrap();
    assert_eq!(from_json.to_binary().unwrap(), store.to_binary().unwrap());
    let back = Store::<World>::from_binary(&from_json.to_binary().unwrap()).unwrap();
    assert_eq!(back.to_json().unwrap(), store.to_json().unwrap());
}

#[test]
fn an_empty_file_is_an_empty_log() {
    let store = Store::<World>::from_binary(&[]).unwrap();
    assert!(store.events().is_empty());
    // Nothing is written for an empty log, so that stays true.
    assert!(store.to_binary().unwrap().is_empty());
}

#[test]
fn it_is_much_smaller_than_json() {
    let mut store = Store::<World>::new();
    let users: Vec<_> =
        (0..50).map(|i| store.create(User { name: format!("user{i:02}"), ..Default::default() })).collect();
    for i in 0..2000 {
        let topic = store.create(Topic { title: format!("topic #{i}"), ..Default::default() });
        store.edit(topic).posts.push(format!("user{:02} says hello ({i})", i % 50));
        store.edit(users[i % 50]).score += 1;
    }
    let json = store.to_json().unwrap().len();
    let binary = store.to_binary().unwrap().len();
    assert!(binary * 100 < json * 40, "binary {binary} bytes, JSON {json}");
}

#[test]
fn appending_gives_the_same_bytes_as_writing_it_all() {
    let full = sample();
    let mut store = Store::<World>::new();
    let mut file = Vec::new();
    let mut saved = 0;
    // Replays the sample a few events at a time, appending after each step.
    for chunk in full.timestamps().iter().zip(full.events()).collect::<Vec<_>>().chunks(3) {
        for &(&at, event) in chunk {
            store.apply_event_at(at, event.clone()).unwrap();
        }
        store.write_binary_since(saved, &mut file).unwrap();
        saved = store.events().len();
    }
    // The names were never repeated, and nothing was reset.
    assert_eq!(file, full.to_binary().unwrap());
}

#[test]
fn a_reopened_log_can_be_appended_to() {
    let mut file = sample().to_binary().unwrap();
    for round in 0..3 {
        let mut store = Store::<World>::read_binary(file.as_slice()).unwrap();
        let saved = store.events().len();
        let (u, _) = store.all::<User>().next().unwrap();
        store.edit(u).notes.insert(format!("round {round}"), "new name, new key".into());
        store.create(Topic { title: format!("round {round}"), ..Default::default() });
        let before = file.len();
        store.write_binary_since(saved, &mut file).unwrap();
        // No reset: a reset is the byte 0 at the start of what was appended.
        assert_ne!(file[before], 0);

        let again = Store::<World>::from_binary(&file).unwrap();
        assert_same(&store, &again);
        assert_eq!(again.to_binary().unwrap(), store.to_binary().unwrap());
    }
}

#[test]
fn appending_to_a_file_the_store_does_not_know_starts_with_a_reset() {
    let full = sample();
    let half = full.events().len() / 2;
    // A file with the first half, written by another store.
    let mut file = full.as_of(half).to_binary().unwrap();
    let before = file.len();
    full.write_binary_since(half, &mut file).unwrap();
    assert_eq!(file[before], 0, "a reset comes first");
    assert_same(&full, &Store::from_binary(&file).unwrap());

    // After writing a second file, appending to the first still works.
    let mut other = Vec::new();
    full.write_binary(&mut other).unwrap();
    let mut store = Store::<World>::from_binary(&file).unwrap();
    let saved = store.events().len();
    store.create(Topic::default());
    store.write_binary(&mut other).unwrap();
    store.write_binary_since(saved, &mut file).unwrap();
    assert_same(&store, &Store::from_binary(&file).unwrap());
}

#[test]
fn a_cut_off_log_is_an_error_or_a_shorter_log() {
    let store = sample();
    let bytes = store.to_binary().unwrap();
    let mut whole_events = 0;
    for end in 0..bytes.len() {
        match Store::<World>::from_binary(&bytes[..end]) {
            // Cut between two events: the events before it.
            Ok(shorter) => {
                assert_eq!(shorter.events(), &store.events()[..shorter.events().len()]);
                whole_events += 1;
            }
            Err(Error::Binary { offset, reason }) => {
                assert!(offset < end);
                if end > 5 {
                    assert!(reason.contains("cut off"), "{reason}");
                }
            }
            Err(e) => panic!("{e}"),
        }
    }
    // Every event ends somewhere, and so does the empty file and the header.
    assert_eq!(whole_events, store.events().len() + 1);
}

#[test]
fn corrupt_logs_give_errors() {
    let bytes = sample().to_binary().unwrap();
    let message = |bytes: &[u8]| Store::<World>::from_binary(bytes).unwrap_err().to_string();

    assert!(message(b"{\"at\":1}").contains("not a replayable binary log"));
    let mut newer = bytes.clone();
    newer[4] = 2;
    assert!(message(&newer).contains("version 2 of the binary format"));
    // The first record: time, a map with one entry, then the name.
    let mut garbled = bytes.clone();
    garbled[5 + 6 + 1] = 0x11;
    assert_eq!(message(&garbled), "invalid binary log at byte 5: unknown tag 0x11");
    // A reference to a name that isn't defined yet.
    let mut garbled = bytes.clone();
    garbled[5 + 6 + 1] = 0x2F;
    assert!(message(&garbled).contains("name #15 is used before it's defined"));
}

#[test]
fn times_are_kept_exactly() {
    let store = sample();
    let back = Store::<World>::from_binary(&store.to_binary().unwrap()).unwrap();
    assert_eq!(back.timestamps(), store.timestamps());
    assert!(store.timestamps().windows(2).any(|w| w[0] == w[1]));
    assert!(store.timestamps().windows(2).any(|w| w[1].unix_millis() - w[0].unix_millis() > 86_400_000));
}
