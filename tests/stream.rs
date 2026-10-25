//! Streaming logs: flushing events out of memory, resuming a log without
//! keeping its events, reading in chunks, and `LogFile`.

use replayable::{model, Error, List, LogFile, Map, Store, Timestamp};
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

model! {
    pub World {
        User { name: String, friends: List<User> }
        Note { text: String, tags: Map<String, u32> }
    }
}

/// A clock that ticks 7 ms per event, starting as if `events` had been
/// logged, so that two stores built the same way log the same times.
fn clock(events: usize) -> impl Fn() -> Timestamp + Send + Sync {
    let ticks = AtomicU64::new(events as u64);
    move || Timestamp::from_unix_millis(1_792_833_302_123 + 7 * ticks.fetch_add(1, Ordering::Relaxed))
}

fn store() -> Store<World> {
    Store::new().with_clock(clock(0))
}

/// One step of building a sample log. Each step defines a new name (a map
/// key), so names are defined all through the log, also in events that
/// straddle two chunks, and later flushes refer to names from earlier ones.
fn step(store: &mut Store<World>, i: usize) {
    let u = store.create(User { name: format!("user {i}"), ..Default::default() });
    store.pin(u);
    let first = store.all::<User>().next().map(|(id, _)| id);
    if let Some(first) = first {
        store.edit(u).friends.push(first);
    }
    let n = store.create(Note { text: "x".repeat(i % 40), ..Default::default() });
    let mut note = store.edit(n);
    note.text.push('!');
    note.tags.insert(format!("tag {i}"), i as u32);
    note.tags.insert(format!("tag {}", i / 2), 0);
    drop(note);
    store.pin(n);
}

/// A log of more than one chunk, with an event larger than a chunk.
fn big() -> Store<World> {
    let mut store = store();
    for i in 0..2000 {
        step(&mut store, i);
    }
    let n = store.create(Note { text: "big".repeat(15_000), ..Default::default() });
    store.pin(n);
    for i in 2000..2100 {
        step(&mut store, i);
    }
    store
}

fn users(store: &Store<World>) -> Vec<(u64, User)> {
    store.all::<User>().map(|(id, u)| (id.raw(), u.clone())).collect()
}

fn notes(store: &Store<World>) -> Vec<(u64, Note)> {
    store.all::<Note>().map(|(id, n)| (id.raw(), n.clone())).collect()
}

#[track_caller]
fn assert_same_state(a: &Store<World>, b: &Store<World>) {
    assert_eq!(users(a), users(b));
    assert_eq!(notes(a), notes(b));
    assert_eq!(a.event_count(), b.event_count());
}

/// Gives at most `n` bytes per read.
struct Trickle<'a>(&'a [u8], usize);

impl Read for Trickle<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.1.min(buf.len()).min(self.0.len());
        buf[..n].copy_from_slice(&self.0[..n]);
        self.0 = &self.0[n..];
        Ok(n)
    }
}

#[test]
fn flushing_in_parts_writes_the_same_log() {
    let mut whole = store();
    let mut flushed = store();
    let (mut binary, mut json) = (Vec::new(), Vec::new());
    let mut flushed_json = store();
    for i in 0..30 {
        step(&mut whole, i);
        step(&mut flushed, i);
        step(&mut flushed_json, i);
        if i % 7 == 0 {
            flushed.flush_binary(&mut binary).unwrap();
            flushed_json.flush_json(&mut json).unwrap();
            assert!(flushed.events().is_empty());
        }
    }
    flushed.flush_binary(&mut binary).unwrap();
    flushed_json.flush_json(&mut json).unwrap();
    // Nothing new: nothing is written.
    let len = binary.len();
    flushed.flush_binary(&mut binary).unwrap();
    assert_eq!(binary.len(), len);

    assert_eq!(binary, whole.to_binary().unwrap());
    assert_eq!(String::from_utf8(json).unwrap(), whole.to_json().unwrap());
    assert_eq!(flushed.event_count(), whole.event_count());
    assert!(flushed.events().is_empty() && flushed.timestamps().is_empty());
}

#[test]
fn a_resumed_log_continues_where_it_left_off() {
    let mut whole = store();
    for i in 0..20 {
        step(&mut whole, i);
    }
    let mut binary = whole.to_binary().unwrap();
    let mut json = whole.to_json().unwrap().into_bytes();

    let mut from_binary = Store::<World>::resume_binary(&binary[..]).unwrap();
    let mut from_json = Store::<World>::resume_json(&json[..]).unwrap();
    for resumed in [&from_binary, &from_json] {
        assert_same_state(resumed, &whole);
        assert!(resumed.events().is_empty());
    }

    // The clock goes on, and so do the ids.
    from_binary = from_binary.with_clock(clock(whole.event_count()));
    from_json = from_json.with_clock(clock(whole.event_count()));
    for i in 20..30 {
        step(&mut whole, i);
        step(&mut from_binary, i);
        step(&mut from_json, i);
    }
    from_binary.flush_binary(&mut binary).unwrap();
    from_json.flush_json(&mut json).unwrap();

    // The names and times carry on from the file, so the bytes are the same
    // as if it had been written in one go.
    assert_eq!(binary, whole.to_binary().unwrap());
    assert_eq!(String::from_utf8(json).unwrap(), whole.to_json().unwrap());
}

#[test]
fn reading_in_chunks_gives_the_same_store() {
    let store = big();
    let bytes = store.to_binary().unwrap();
    assert!(bytes.len() > 100_000, "{} bytes", bytes.len());
    let in_memory = Store::<World>::from_binary(&bytes).unwrap();
    for n in [1, 3, 1000, usize::MAX] {
        let streamed = Store::<World>::read_binary(Trickle(&bytes, n)).unwrap();
        assert_eq!(streamed.events(), in_memory.events());
        assert_eq!(streamed.timestamps(), in_memory.timestamps());
        let resumed = Store::<World>::resume_binary(Trickle(&bytes, n)).unwrap();
        assert_same_state(&resumed, &in_memory);
        assert!(resumed.events().is_empty());
    }
    in_memory.verify().unwrap();
}

#[test]
fn reset_records_are_read_in_chunks() {
    let mut store = big();
    let mut file = store.to_binary().unwrap();
    let saved = store.event_count();
    for i in 0..10 {
        step(&mut store, i);
    }
    // The store doesn't know what names a file it only wrote holds, so the
    // append starts with a reset.
    store.write_binary_since(saved, &mut file).unwrap();
    let streamed = Store::<World>::read_binary(Trickle(&file, 5)).unwrap();
    assert_eq!(streamed.events(), store.events());
}

#[test]
fn error_offsets_count_from_the_start_of_the_log() {
    let mut bytes = big().to_binary().unwrap();
    let at = bytes.len() as u64;
    // An event, 0 ms after the last one, with an unknown tag.
    bytes.extend([0x01, 0x11]);
    for result in [Store::<World>::from_binary(&bytes), Store::<World>::read_binary(Trickle(&bytes, 4096))] {
        match result {
            Err(Error::Binary { offset, reason }) => {
                assert_eq!(offset, at);
                assert!(reason.contains("unknown tag 0x11"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn a_loaded_log_isnt_flushed_again() {
    let mut store = store();
    step(&mut store, 0);
    let mut file = store.to_binary().unwrap();
    let mut loaded = Store::<World>::from_binary(&file).unwrap();
    assert_eq!(loaded.events().len(), loaded.event_count());
    loaded.flush_binary(&mut file).unwrap();
    assert_eq!(file, store.to_binary().unwrap());
    assert!(loaded.events().is_empty());
}

#[test]
fn flush_with_keeps_what_it_couldnt_store() {
    let mut store = store();
    step(&mut store, 0);
    let all = store.events().to_vec();
    let mut got = Vec::new();
    let result = store.flush_with(|_, event| {
        if got.len() == 2 {
            return Err("full");
        }
        got.push(event.clone());
        Ok(())
    });
    assert_eq!(result, Err("full"));
    assert_eq!(store.events(), &all[2..]);
    assert_eq!(store.event_count(), all.len());
    assert_eq!(store.events_since(2), &all[2..]);
    store
        .flush_with(|_, event| {
            got.push(event.clone());
            Ok::<_, ()>(())
        })
        .unwrap();
    assert_eq!(got, all);
    assert!(store.events().is_empty());
}

/// A store that forgot the events of the first step, but not those of the
/// second, and how many it forgot.
fn forgotten() -> (Store<World>, usize) {
    let mut store = store();
    step(&mut store, 0);
    let n = store.event_count();
    store.forget_events();
    step(&mut store, 1);
    (store, n)
}

#[test]
fn forgotten_events_are_gone_from_memory() {
    let (store, n) = forgotten();
    assert_eq!(store.event_count(), n + store.events().len());
    assert_eq!(store.events_since(n), store.events());
    assert!(store.verify().unwrap_err().contains("forgotten"));
    let _ = store.as_of(0);
}

#[test]
#[should_panic(expected = "were forgotten")]
fn as_of_needs_the_whole_log() {
    let _ = forgotten().0.as_of(1);
}

#[test]
#[should_panic(expected = "were forgotten")]
fn to_binary_needs_the_whole_log() {
    let _ = forgotten().0.to_binary();
}

#[test]
#[should_panic(expected = "were forgotten")]
fn to_json_needs_the_whole_log() {
    let _ = forgotten().0.to_json();
}

#[test]
#[should_panic(expected = "were forgotten")]
fn write_json_since_a_forgotten_event_panics() {
    let (store, n) = forgotten();
    let _ = store.write_json_since(n - 1, Vec::new());
}

/// A path in the temporary directory, removed first.
fn temp(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("replayable-{}-{name}.rplb", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

#[test]
fn a_log_file_is_created_appended_and_reopened() {
    let path = temp("reopen");
    let mut whole = store();
    {
        let (mut file, store) = LogFile::open::<World>(&path).unwrap();
        assert!(file.is_empty() && path.exists());
        assert_eq!(store.event_count(), 0);
        let mut store = store.with_clock(clock(0));
        for i in 0..5 {
            step(&mut store, i);
            step(&mut whole, i);
            file.append(&mut store).unwrap();
            assert!(store.events().is_empty());
        }
        file.sync().unwrap();
        assert_eq!(file.len(), std::fs::metadata(&path).unwrap().len());
    }
    let (mut file, mut store) = LogFile::open::<World>(&path).unwrap();
    assert_eq!(file.truncated(), 0);
    assert_same_state(&store, &whole);
    store = store.with_clock(clock(whole.event_count()));
    step(&mut store, 5);
    step(&mut whole, 5);
    file.append(&mut store).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), whole.to_binary().unwrap());
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn a_log_file_cut_off_in_an_event_loses_only_that_event() {
    let path = temp("cut");
    let mut before = store();
    step(&mut before, 0);
    let whole = before.to_binary().unwrap();
    let mut shorter = before.as_of(before.event_count() - 1);
    let keep = shorter.to_binary().unwrap();
    shorter.forget_events();
    for end in keep.len() + 1..whole.len() {
        std::fs::write(&path, &whole[..end]).unwrap();
        let (file, store) = LogFile::open::<World>(&path).unwrap();
        assert_eq!(file.truncated(), (end - keep.len()) as u64);
        assert_eq!(std::fs::read(&path).unwrap(), keep);
        assert_same_state(&store, &shorter);
    }
    // Cut off in the header: an empty log.
    for end in 1..5 {
        std::fs::write(&path, &whole[..end]).unwrap();
        let (file, store) = LogFile::open::<World>(&path).unwrap();
        assert_eq!((file.truncated(), file.len(), store.event_count()), (end as u64, 0, 0));
    }
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn a_log_file_that_is_corrupt_is_an_error() {
    let path = temp("corrupt");
    let mut store = store();
    step(&mut store, 0);
    let mut bytes = store.to_binary().unwrap();
    bytes.extend([0x01, 0x11]);
    std::fs::write(&path, &bytes).unwrap();
    assert!(matches!(LogFile::open::<World>(&path), Err(Error::Binary { .. })));
    // Not cut off.
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    std::fs::write(&path, b"not a log").unwrap();
    assert!(matches!(LogFile::open::<World>(&path), Err(Error::Binary { offset: 0, .. })));
    std::fs::remove_file(&path).unwrap();
}
