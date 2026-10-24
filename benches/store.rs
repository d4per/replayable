//! Rough timings of common operations: `cargo bench`.
//!
//! A small harness with no dependencies. Each case runs its setup outside
//! the timing, and the median of several runs is reported.

use replayable::{model, Id, List, Store};
use std::hint::black_box;
use std::time::{Duration, Instant};

model! {
    Bench {
        User {
            name: String,
            age: u32,
            tags: List<u32>,
        }
    }
}

const N: u32 = 10_000;
const RUNS: usize = 15;
const WARMUP: usize = 3;

fn bench<S, R>(name: &str, mut setup: impl FnMut() -> S, mut routine: impl FnMut(S) -> R) {
    let mut times: Vec<Duration> = (0..WARMUP + RUNS)
        .map(|_| {
            let input = setup();
            let start = Instant::now();
            let output = routine(input);
            let elapsed = start.elapsed();
            drop(black_box(output));
            elapsed
        })
        .skip(WARMUP)
        .collect();
    times.sort();
    println!("{name:<32} {:>12.3?}", times[RUNS / 2]);
}

fn users(n: u32) -> (Store<Bench>, Vec<Id<User>>) {
    let mut store = Store::new();
    let ids = (0..n).map(|i| store.create(User { name: format!("user {i}"), age: i, ..Default::default() })).collect();
    (store, ids)
}

fn user_with_tags(tags: impl IntoIterator<Item = u32>) -> (Store<Bench>, Id<User>) {
    let mut store = Store::new();
    let id = store.create(User { tags: tags.into_iter().collect(), ..Default::default() });
    (store, id)
}

/// A log of 10k users followed by edits of scalars and lists.
fn big_log() -> Store<Bench> {
    let (mut store, ids) = users(N);
    for round in 0..3 {
        for (i, &id) in ids.iter().enumerate() {
            let mut u = store.edit(id);
            u.age += 1;
            u.tags.push(i as u32 + round);
        }
    }
    store
}

fn main() {
    bench("create 10k", || (), |_| users(N));

    bench(
        "edit scalar 10k",
        || users(N),
        |(mut store, ids)| {
            for &id in &ids {
                store.edit(id).age += 1;
            }
            store
        },
    );

    bench(
        "push 10k, one edit each",
        || user_with_tags([]),
        |(mut store, id)| {
            for i in 0..N {
                store.edit(id).tags.push(i);
            }
            store
        },
    );

    bench(
        "extend 10k in one edit",
        || user_with_tags([]),
        |(mut store, id)| {
            store.edit(id).tags.extend(0..N);
            store
        },
    );

    bench(
        "sort reversed 10k",
        || user_with_tags((0..N).rev()),
        |(mut store, id)| {
            store.edit(id).tags.sort();
            store
        },
    );

    bench(
        "sort 1k, 30 out of place",
        || user_with_tags((0..1000).map(|i| if i % 33 == 0 { 1000 - i } else { i })),
        |(mut store, id)| {
            store.edit(id).tags.sort();
            store
        },
    );

    bench(
        "retain half of 10k",
        || user_with_tags(0..N),
        |(mut store, id)| {
            store.edit(id).tags.retain(|t| t % 2 == 0);
            store
        },
    );

    let log = big_log();
    let json = log.to_json().unwrap();
    let binary = log.to_binary().unwrap();
    println!(
        "(big log: {} events, {} KiB of JSON, {} KiB binary)",
        log.events().len(),
        json.len() / 1024,
        binary.len() / 1024
    );
    bench("replay big log", || log.events().to_vec(), |events| Store::<Bench>::replay(events).unwrap());
    bench("to_json big log", || (), |_| log.to_json().unwrap());
    bench("from_json big log", || (), |_| Store::<Bench>::from_json(&json).unwrap());
    bench("to_binary big log", || (), |_| log.to_binary().unwrap());
    bench("from_binary big log", || (), |_| Store::<Bench>::from_binary(&binary).unwrap());

    // How many events each list operation logs.
    let count = |f: &dyn Fn(&mut List<u32>)| {
        let (mut store, id) = user_with_tags((0..N).rev());
        let before = store.events().len();
        f(&mut store.edit(id).tags);
        store.events().len() - before
    };
    println!(
        "events logged: extend 10k = {}, sort reversed 10k = {}, truncate to 0 = {}",
        count(&|t| t.extend(0..N)),
        count(&|t| t.sort()),
        count(&|t| t.truncate(0))
    );
}
