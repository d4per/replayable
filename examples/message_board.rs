//! A message board that uses its event log as the database.
//!
//! Users create topics and post messages. A message can be deleted by its
//! author, as long as no other user has posted in the topic after it.
//!
//! Users and topics are pinned. A message is kept because its topic lists
//! it, so deleting one means taking it out of the topic and calling
//! `collect`.
//!
//! 100 users act at the same time, each in its own thread. Every operation
//! runs under one lock (the store has a single writer), and the events it
//! produced are appended to a JSON Lines file before the lock is released.
//! At startup the file is replayed, so running the example again continues
//! the same board. At the end, the file is replayed from scratch and
//! compared with the live state.
//!
//! The log is JSON Lines, or the compact binary format with `--binary`.
//!
//! ```text
//! cargo run --release --example message_board                 # continue the board
//! cargo run --release --example message_board -- --fresh      # start over
//! cargo run --release --example message_board -- --binary     # use the binary format
//! cargo run --release --example message_board -- board.jsonl  # another file
//! ```

use replayable::{model, Id, List, Store};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

model! {
    pub Board {
        User {
            name: String,
        }
        Topic {
            title: String,
            author: User,
            /// The messages in the order they were posted.
            messages: List<Message>,
        }
        Message {
            topic: Topic,
            author: User,
            text: String,
        }
    }
}

const USERS: usize = 100;
const ACTIONS_PER_USER: usize = 50;

/// Why a message couldn't be deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refused {
    NotTheAuthor,
    OthersPostedAfter,
    AlreadyGone,
}

/// The store, and the log file it's saved to.
struct Db {
    store: Store<Board>,
    file: BufWriter<File>,
    /// Whether the file is in the binary format rather than JSON Lines.
    binary: bool,
}

/// Reads a log file in either format.
fn load(path: &Path, binary: bool) -> Result<Store<Board>, replayable::Error> {
    let file = BufReader::new(File::open(path)?);
    if binary {
        Store::read_binary(file)
    } else {
        Store::read_json(file)
    }
}

impl Db {
    /// Opens the log at `path`, replaying it if it exists.
    fn open(path: &Path, binary: bool) -> Result<Db, Box<dyn std::error::Error>> {
        let store = if path.exists() { load(path, binary)? } else { Store::new() };
        let file = BufWriter::new(OpenOptions::new().create(true).append(true).open(path)?);
        let mut db = Db { store, file, binary };

        // Files from before pins existed: pin the roots, or `collect` would
        // delete them.
        let users: Vec<Id<User>> = db.store.all::<User>().map(|(id, _)| id).collect();
        let topics: Vec<Id<Topic>> = db.store.all::<Topic>().map(|(id, _)| id).collect();
        let pinned = users.into_iter().filter(|&u| db.store.pin(u)).count()
            + topics.into_iter().filter(|&t| db.store.pin(t)).count();
        if pinned > 0 {
            println!("pinned {pinned} users and topics from a log written before pins");
            db.commit();
        }
        Ok(db)
    }

    /// Appends the events not yet in the file. Called at the end of every
    /// operation, while the lock is still held, so the file always holds
    /// whole operations in the order they happened.
    fn commit(&mut self) {
        if self.binary {
            self.store.save_binary(&mut self.file).expect("writing the log");
        } else {
            self.store.save_json(&mut self.file).expect("writing the log");
        }
        self.file.flush().expect("writing the log");
    }

    fn create_user(&mut self, name: String) -> Id<User> {
        let id = self.store.create(User { name });
        self.store.pin(id);
        self.commit();
        id
    }

    fn create_topic(&mut self, author: Id<User>, title: String) -> Id<Topic> {
        let id = self.store.create(Topic { title, author: Some(author), ..Default::default() });
        self.store.pin(id);
        self.commit();
        id
    }

    fn post(&mut self, author: Id<User>, topic: Id<Topic>, text: String) -> Id<Message> {
        let message = self.store.create(Message { topic: Some(topic), author: Some(author), text });
        self.store.edit(topic).messages.push(message);
        self.commit();
        message
    }

    /// Deletes `message` if `user` wrote it and no one else has posted in
    /// the topic since. The check and the delete happen under the same lock,
    /// so nobody can post in between.
    fn delete_message(&mut self, user: Id<User>, message: Id<Message>) -> Result<(), Refused> {
        let store = &self.store;
        let msg = store.get(message).ok_or(Refused::AlreadyGone)?;
        if msg.author != Some(user) {
            return Err(Refused::NotTheAuthor);
        }
        let topic = msg.topic.expect("every message has a topic");
        let messages = &store[topic].messages;
        let pos = messages.iter().position(|&m| m == message).expect("a message is in its topic");
        if messages[pos + 1..].iter().any(|&later| store[later].author != Some(user)) {
            return Err(Refused::OthersPostedAfter);
        }

        // Nothing else refers to the message, so `collect` deletes it.
        self.store.edit(topic).messages.remove(pos);
        assert_eq!(self.store.collect(), 1);
        self.commit();
        Ok(())
    }
}

/// A small random number generator, so the example needs no dependencies.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n.max(1) as u64) as usize
    }
}

#[derive(Default)]
struct Tally {
    topics: usize,
    posts: usize,
    deleted: usize,
    refused_others_posted: usize,
    refused_not_author: usize,
    refused_gone: usize,
}

impl Tally {
    fn add(&mut self, other: &Tally) {
        self.topics += other.topics;
        self.posts += other.posts;
        self.deleted += other.deleted;
        self.refused_others_posted += other.refused_others_posted;
        self.refused_not_author += other.refused_not_author;
        self.refused_gone += other.refused_gone;
    }
}

/// What one user does: mostly posting, sometimes a new topic, sometimes
/// trying to delete one of their own messages (or, now and then, someone
/// else's, which must be refused).
fn run_user(db: &Mutex<Db>, user: Id<User>, seed: u64) -> Tally {
    let mut rng = Rng(seed | 1);
    let mut tally = Tally::default();
    let name = db.lock().unwrap().store[user].name.clone();

    // Messages this user wrote in earlier runs can be deleted too.
    let mut mine: Vec<Id<Message>> = {
        let db = db.lock().unwrap();
        db.store.all::<Message>().filter(|(_, m)| m.author == Some(user)).map(|(id, _)| id).collect()
    };

    for action in 0..ACTIONS_PER_USER {
        let roll = rng.below(100);
        let mut db = db.lock().unwrap();
        let topics = db.store.count::<Topic>();

        if roll < 8 || topics == 0 {
            db.create_topic(user, format!("{name}'s topic #{action}"));
            tally.topics += 1;
        } else if roll < 80 {
            let (topic, _) = db.store.all::<Topic>().nth(rng.below(topics)).unwrap();
            mine.push(db.post(user, topic, format!("{name} says hello ({action})")));
            tally.posts += 1;
        } else if roll < 97 && !mine.is_empty() {
            let i = rng.below(mine.len());
            match db.delete_message(user, mine[i]) {
                Ok(()) => {
                    mine.swap_remove(i);
                    tally.deleted += 1;
                }
                Err(Refused::OthersPostedAfter) => tally.refused_others_posted += 1,
                Err(Refused::AlreadyGone) => {
                    mine.swap_remove(i);
                    tally.refused_gone += 1;
                }
                Err(Refused::NotTheAuthor) => unreachable!("these are our own messages"),
            }
        } else {
            // Try to delete someone else's message: always refused.
            let messages = db.store.count::<Message>();
            if messages > 0 {
                let (message, msg) = db.store.all::<Message>().nth(rng.below(messages)).unwrap();
                if msg.author != Some(user) {
                    assert_eq!(db.delete_message(user, message), Err(Refused::NotTheAuthor));
                    tally.refused_not_author += 1;
                }
            }
        }
        drop(db);

        // Let the other users get in between.
        std::thread::sleep(Duration::from_micros(rng.below(300) as u64));
    }
    tally
}

/// Checks the rules the board must always follow.
fn check_invariants(store: &Store<Board>) {
    let mut listed = 0;
    for (topic_id, topic) in store.all::<Topic>() {
        assert!(store.contains(topic.author.unwrap()), "{topic_id:?} has an author");
        for &m in &topic.messages {
            let message = store.get(m).unwrap_or_else(|| panic!("{topic_id:?} lists deleted {m:?}"));
            assert_eq!(message.topic, Some(topic_id), "{m:?} is listed in its own topic");
            listed += 1;
        }
    }
    assert_eq!(listed, store.count::<Message>(), "every message is listed in exactly one topic");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let fresh = args.iter().any(|a| a == "--fresh");
    let binary = args.iter().any(|a| a == "--binary");
    let default_name = if binary { "replayable_message_board.rplb" } else { "replayable_message_board.jsonl" };
    let path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(default_name));
    if fresh {
        let _ = std::fs::remove_file(&path);
    }

    let started = Instant::now();
    let mut db = Db::open(&path, binary)?;
    let replayed = db.store.events().len();
    println!("log: {}", path.display());
    println!("replayed {replayed} events in {:.1?}", started.elapsed());

    // The same 100 users every run: create the ones that aren't there yet.
    let existing: Vec<(Id<User>, String)> = db.store.all::<User>().map(|(id, u)| (id, u.name.clone())).collect();
    let users: Vec<Id<User>> = (0..USERS)
        .map(|i| {
            let name = format!("user{i:02}");
            match existing.iter().find(|(_, n)| *n == name) {
                Some(&(id, _)) => id,
                None => db.create_user(name),
            }
        })
        .collect();

    // All users at once.
    let run = Instant::now();
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos() as u64;
    let db = Mutex::new(db);
    let mut total = Tally::default();
    std::thread::scope(|s| {
        let threads: Vec<_> = users
            .iter()
            .enumerate()
            .map(|(i, &user)| {
                let db = &db;
                s.spawn(move || run_user(db, user, seed.wrapping_mul(i as u64 + 1)))
            })
            .collect();
        for thread in threads {
            total.add(&thread.join().expect("a user thread panicked"));
        }
    });
    let db = db.into_inner().unwrap();
    let elapsed = run.elapsed();

    println!(
        "\n{USERS} users, {ACTIONS_PER_USER} actions each, in {elapsed:.1?}:\n  \
         {} topics created, {} messages posted, {} deleted\n  \
         refused deletes: {} because others had posted after, {} not the author's, {} already gone",
        total.topics,
        total.posts,
        total.deleted,
        total.refused_others_posted,
        total.refused_not_author,
        total.refused_gone,
    );

    let store = &db.store;
    check_invariants(store);
    println!(
        "\nboard now: {} users, {} topics, {} messages, {} events in the log ({} new this run)",
        store.count::<User>(),
        store.count::<Topic>(),
        store.count::<Message>(),
        store.events().len(),
        store.events().len() - replayed,
    );

    // The busiest topic, as a sample.
    if let Some((_, topic)) = store.all::<Topic>().max_by_key(|(_, t)| t.messages.len()) {
        println!("\nbusiest topic: \"{}\" by {}", topic.title, store[topic.author.unwrap()].name);
        for &m in topic.messages.iter().rev().take(5).rev() {
            println!("  {}: {}", store[store[m].author.unwrap()].name, store[m].text);
        }
        if topic.messages.len() > 5 {
            println!("  ... and {} earlier messages", topic.messages.len() - 5);
        }
    }

    // The file is the database: replay it from scratch and compare.
    let reload = Instant::now();
    let from_file = load(&path, binary)?;
    let reload_time = reload.elapsed();
    check_invariants(&from_file);
    assert_eq!(from_file.events().len(), store.events().len());
    for (id, topic) in store.all::<Topic>() {
        assert_eq!(&from_file[id], topic);
    }
    for (id, message) in store.all::<Message>() {
        assert_eq!(&from_file[id], message);
    }
    for (id, user) in store.all::<User>() {
        assert_eq!(&from_file[id], user);
    }
    assert_eq!(from_file.count::<Message>(), store.count::<Message>());
    store.verify()?;
    let size = std::fs::metadata(&path)?.len();
    println!("\nreplayed the log file ({} KiB) in {reload_time:.1?}: identical to the live state ✔", size / 1024);

    // The same log in both formats.
    let json = store.to_json()?;
    let bytes = store.to_binary()?;
    let timed = |load: &dyn Fn() -> Store<Board>| {
        let start = Instant::now();
        let loaded = load();
        assert_eq!(loaded.events(), store.events());
        assert_eq!(loaded.timestamps(), store.timestamps());
        start.elapsed()
    };
    let json_time = timed(&|| Store::from_json(&json).unwrap());
    let binary_time = timed(&|| Store::from_binary(&bytes).unwrap());
    println!(
        "the same log as JSON: {} KiB, loads in {json_time:.1?}; binary: {} KiB ({:.0}%), loads in {binary_time:.1?}",
        json.len() / 1024,
        bytes.len() / 1024,
        100.0 * bytes.len() as f64 / json.len() as f64,
    );

    show_deleted_history(store)?;
    Ok(())
}

/// Deleting a message adds events and removes none: the post stays in the
/// log, followed by its removal from the topic and the delete logged by
/// `collect`. So the
/// board can be rebuilt as it was before any deletion.
fn show_deleted_history(store: &Store<Board>) -> Result<(), replayable::Error> {
    // Walk the log once, checking that every deleted message was posted
    // earlier and still existed right before it was deleted.
    let times = store.timestamps();
    let mut timeline = Store::<Board>::new();
    let mut deletions = Vec::new();
    for (i, event) in store.events().iter().enumerate() {
        if let BoardEvent::DeleteMessage(m) = event {
            assert!(timeline.contains(*m), "{m:?} existed until it was deleted");
            // A delete is two events: the removal from the topic, then this.
            assert!(matches!(store.events()[i - 1], BoardEvent::UpdateTopic(..)));
            deletions.push((i - 1, *m));
        }
        timeline.apply_event_at(times[i], event.clone())?;
    }
    println!("\n{} deletions in the whole log, each one after its post ✔", deletions.len());

    // Go back to just before the latest deletion.
    let Some(&(deleted_at, message)) = deletions.last() else { return Ok(()) };
    let posted_at = store
        .events()
        .iter()
        .position(|e| matches!(e, BoardEvent::CreateMessage(m, _) if *m == message))
        .expect("the post is in the log");
    let before = store.as_of(deleted_at);
    let msg = &before[message];
    let topic = msg.topic.unwrap();
    println!("the latest deleted message, rebuilt with store.as_of({deleted_at}):");
    println!(
        "  event {posted_at}, {}: {} posted \"{}\" in \"{}\"",
        times[posted_at],
        before[msg.author.unwrap()].name,
        msg.text,
        before[topic].title
    );
    println!(
        "  event {deleted_at}, {}: deleted ({}), then {}",
        times[deleted_at],
        store.events()[deleted_at],
        store.events()[deleted_at + 1]
    );
    println!(
        "  the topic had {} messages before, {} after; the message is {} now",
        before[topic].messages.len(),
        store.as_of(deleted_at + 2)[topic].messages.len(),
        if store.contains(message) { "still there?!" } else { "gone" },
    );
    assert!(before[topic].messages.contains(&message));
    assert!(!store.contains(message));

    // The same by time: the board as it was when the message was posted
    // (unless it was deleted in the same millisecond).
    if times[posted_at] < times[deleted_at] {
        assert!(store.as_of_time(times[posted_at])[topic].messages.contains(&message));
        println!("  store.as_of_time({}) has it too ✔", times[posted_at]);
    }
    Ok(())
}
