//! Pins, `collect`, and weak references: what's kept, what's deleted, and
//! how it's logged.

use replayable::{model, Error, Id, List, Map, Store, Value, Weak};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// A value type that holds an id, and says so.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
struct Vote {
    by: Option<Id<Folder>>,
}
impl Value for Vote {
    fn refs(&self, visit: &mut dyn FnMut(u64)) {
        self.by.refs(visit)
    }
}

model! {
    pub Files {
        Folder {
            name: String,
            parent: Folder,
            children: List<Folder>,
            items: List<Item>,
            by_name: Map<String, Item>,
            owners: Map<Item, String>,
        }
        Item {
            name: String,
            folder: Folder,
            link: Weak<Item>,
            seen: List<Weak<Folder>>,
            marks: Map<Weak<Item>, u8>,
            vote: Vote,
        }
    }
}

fn folder(store: &mut Store<Files>, name: &str) -> Id<Folder> {
    store.create(Folder { name: name.into(), ..Default::default() })
}

fn item(store: &mut Store<Files>, name: &str) -> Id<Item> {
    store.create(Item { name: name.into(), ..Default::default() })
}

fn log_since(store: &Store<Files>, n: usize) -> Vec<String> {
    store.events_since(n).iter().map(|e| e.to_string()).collect()
}

#[test]
fn unreachable_cycles_are_collected() {
    let mut store = Store::<Files>::new();
    let root = folder(&mut store, "root");
    store.pin(root);

    // A folder and an item that point to each other.
    let f = folder(&mut store, "f");
    let i = item(&mut store, "i");
    store.edit(f).items.push(i);
    store.edit(i).folder = Some(f);

    // A folder that is its own parent and child.
    let me = folder(&mut store, "me");
    store.edit(me).parent = Some(me);
    store.edit(me).children.push(me);

    // a -> b -> c -> a
    let (a, b, c) = (folder(&mut store, "a"), folder(&mut store, "b"), folder(&mut store, "c"));
    store.edit(a).children.push(b);
    store.edit(b).children.push(c);
    store.edit(c).children.push(a);

    let before = store.events().len();
    assert_eq!(store.collect(), 6);
    assert_eq!(store.count::<Folder>(), 1);
    assert_eq!(store.count::<Item>(), 0);
    assert_eq!(
        log_since(&store, before),
        [
            "delete Folder id:2",
            "delete Folder id:4",
            "delete Folder id:5",
            "delete Folder id:6",
            "delete Folder id:7",
            "delete Item id:3",
        ]
    );
    assert_eq!(store.collect(), 0);
    assert_eq!(store.verify(), Ok(()));
}

#[test]
fn cycles_reachable_from_a_pin_are_kept() {
    let mut store = Store::<Files>::new();
    let root = folder(&mut store, "root");
    store.pin(root);
    let f = folder(&mut store, "f");
    let i = item(&mut store, "i");
    store.edit(root).children.push(f);
    store.edit(f).items.push(i);
    store.edit(i).folder = Some(f);
    store.edit(f).parent = Some(root);
    assert_eq!(store.collect(), 0);

    // The cycle back to the root doesn't keep `f` once the root lets go.
    store.edit(root).children.clear();
    assert_eq!(store.collect(), 2);
    assert!(!store.contains(f) && !store.contains(i));
    assert_eq!(store.verify(), Ok(()));
}

#[test]
fn map_keys_and_values_keep_entities() {
    let mut store = Store::<Files>::new();
    let root = folder(&mut store, "root");
    store.pin(root);
    let (key, value) = (item(&mut store, "key"), item(&mut store, "value"));
    store.edit(root).owners.insert(key, "me".into());
    store.edit(root).by_name.insert("v".into(), value);
    assert_eq!(store.collect(), 0);

    store.edit(root).owners.clear();
    assert_eq!(store.collect(), 1);
    assert!(!store.contains(key) && store.contains(value));
}

#[test]
fn ids_in_value_types_keep_entities() {
    let mut store = Store::<Files>::new();
    let root = folder(&mut store, "root");
    let (i, voter) = (item(&mut store, "i"), folder(&mut store, "voter"));
    store.edit(root).items.push(i);
    store.edit(i).vote = Vote { by: Some(voter) };
    store.pin(root);
    assert_eq!(store.collect(), 0);

    store.edit(i).vote = Vote::default();
    assert_eq!(store.collect(), 1);
    assert!(!store.contains(voter));
}

#[test]
fn unpinning_lets_go() {
    let mut store = Store::<Files>::new();
    let a = folder(&mut store, "a");
    let b = folder(&mut store, "b");
    assert!(store.pin(a));
    assert!(!store.pin(a));
    assert!(store.pin(b));
    assert!(store.is_pinned(a));
    assert_eq!(store.collect(), 0);

    assert!(store.unpin(a));
    assert!(!store.unpin(a));
    assert!(!store.is_pinned(a));
    assert_eq!(store.collect(), 1);
    assert!(!store.contains(a) && store.contains(b));

    let replayed = Store::<Files>::replay(store.events().to_vec()).unwrap();
    assert!(replayed.is_pinned(b) && !replayed.contains(a));
    assert_eq!(store.verify(), Ok(()));
}

#[test]
fn weak_references_are_cleared_before_the_delete() {
    let mut store = Store::<Files>::new();
    let root = folder(&mut store, "root");
    store.pin(root);
    let keep = item(&mut store, "keep");
    store.edit(root).items.push(keep);
    let gone_item = item(&mut store, "gone");
    let gone_folder = folder(&mut store, "gone");
    {
        let mut k = store.edit(keep);
        k.link = Some(gone_item);
        k.seen.extend([root, gone_folder]);
        k.marks.insert(gone_item, 1);
        k.marks.insert(keep, 2);
    }

    let before = store.events().len();
    assert_eq!(store.collect(), 2);
    assert_eq!(
        log_since(&store, before),
        [
            "Item id:2 set link=None",
            "Item id:2 seen.remove(1)",
            "Item id:2 marks.remove(Item id:3)",
            "delete Folder id:4",
            "delete Item id:3",
        ]
    );
    assert_eq!(store[keep].link, None);
    assert_eq!(store[keep].seen.as_slice(), [root]);
    assert_eq!(store[keep].marks.keys().copied().collect::<Vec<_>>(), [keep]);
    assert_eq!(store.verify(), Ok(()));
}

#[test]
fn a_weak_reference_does_not_keep_anything() {
    let mut store = Store::<Files>::new();
    let root = folder(&mut store, "root");
    store.pin(root);
    let i = item(&mut store, "i");
    store.edit(root).items.push(i);
    let other = item(&mut store, "other");
    store.edit(i).link = Some(other);
    assert_eq!(store.collect(), 1);
    assert!(!store.contains(other));

    // But a weak reference to something kept is left alone.
    store.edit(i).link = Some(i);
    assert_eq!(store.collect(), 0);
    assert_eq!(store[i].link, Some(i));
}

#[test]
fn collecting_an_empty_store_is_fine() {
    let mut store = Store::<Files>::new();
    assert_eq!(store.collect(), 0);
}

#[test]
#[should_panic(expected = "nothing is pinned")]
fn collecting_without_pins_panics() {
    let mut store = Store::<Files>::new();
    folder(&mut store, "a");
    store.collect();
}

#[test]
#[should_panic(expected = "Folder id:9 doesn't exist")]
fn pinning_a_missing_entity_panics() {
    Store::<Files>::new().pin(Id::<Folder>::from_raw(9));
}

#[test]
fn bad_pins_in_a_log_are_errors() {
    let create = r#"{"at":"2026-10-24T09:15:02.123Z","CreateFolder":[1,{}]}"#;
    let pin = r#"{"at":"2026-10-24T09:15:02.123Z","PinFolder":1}"#;
    let unpin = r#"{"at":"2026-10-24T09:15:02.123Z","UnpinFolder":1}"#;
    for (log, reason) in [
        (pin.to_string(), "Folder id:1 doesn't exist"),
        (format!("{create}\n{pin}\n{pin}"), "Folder id:1 is already pinned"),
        (format!("{create}\n{unpin}"), "Folder id:1 isn't pinned"),
    ] {
        match Store::<Files>::from_json(&log) {
            Err(Error::InvalidEvent { reason: r, .. }) => assert_eq!(r, reason),
            other => panic!("{log}: {other:?}"),
        }
    }
}

#[test]
fn verify_finds_references_to_entities_that_dont_exist() {
    let mut store = Store::<Files>::new();
    let root = folder(&mut store, "root");
    store.pin(root);
    let old = folder(&mut store, "old");
    assert_eq!(store.collect(), 1);
    assert_eq!(store.verify(), Ok(()));
    // An id kept from before the collect.
    store.edit(root).children.push(old);
    assert_eq!(store.verify(), Err("Folder id:1 refers to id 2, which doesn't exist".into()));
    store.edit(root).children.clear();
    let i = item(&mut store, "i");
    store.edit(root).items.push(i);
    store.edit(i).seen.push(old);
    assert_eq!(store.verify(), Err("Item id:3 has a weak reference to an entity that doesn't exist".into()));
}

#[test]
fn ids_are_unique_across_types_in_a_log() {
    let log = concat!(
        r#"{"at":"2026-10-24T09:15:02.123Z","CreateFolder":[1,{}]}"#,
        "\n",
        r#"{"at":"2026-10-24T09:15:02.123Z","CreateItem":[1,{}]}"#,
    );
    match Store::<Files>::from_json(log) {
        Err(Error::InvalidEvent { index: 1, reason, .. }) => {
            assert_eq!(reason, "Item id:1 can't be created, since an entity of another type has the id 1")
        }
        other => panic!("{other:?}"),
    }
}

/// A log written when entities were deleted by hand still loads, and a
/// deleted entity is no longer pinned.
#[test]
fn explicit_deletes_in_old_logs_still_load() {
    let log = concat!(
        r#"{"at":"2026-10-24T09:15:02.123Z","CreateFolder":[1,{}]}"#,
        "\n",
        r#"{"at":"2026-10-24T09:15:02.123Z","PinFolder":1}"#,
        "\n",
        r#"{"at":"2026-10-24T09:15:02.124Z","DeleteFolder":1}"#,
    );
    let store = Store::<Files>::from_json(log).unwrap();
    assert!(!store.contains(Id::<Folder>::from_raw(1)));
    assert!(!store.is_pinned(Id::<Folder>::from_raw(1)));
    assert_eq!(store.verify(), Ok(()));
}

struct Rng(u64);
impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) % n.max(1) as u64) as usize
    }
}

/// Every id an entity holds, strong or weak.
fn all_ids(store: &Store<Files>) -> BTreeSet<u64> {
    let mut ids = BTreeSet::new();
    for (_, f) in store.all::<Folder>() {
        ids.extend(f.parent.map(|p| p.raw()));
        ids.extend(f.children.iter().map(|c| c.raw()));
        ids.extend(f.items.iter().map(|i| i.raw()));
        ids.extend(f.by_name.values().map(|i| i.raw()));
        ids.extend(f.owners.keys().map(|i| i.raw()));
    }
    for (_, i) in store.all::<Item>() {
        ids.extend(i.folder.map(|f| f.raw()));
        ids.extend(i.link.map(|l| l.raw()));
        ids.extend(i.seen.iter().map(|f| f.raw()));
        ids.extend(i.marks.keys().map(|k| k.raw()));
        ids.extend(i.vote.by.map(|f| f.raw()));
    }
    ids
}

/// Random graphs of folders and items, linked, unlinked, pinned and
/// collected. After every collect nothing dangles, and a second collect
/// finds nothing.
#[test]
fn random_graphs_never_dangle_after_collect() {
    let mut rng = Rng(4242);
    let mut store = Store::<Files>::new();
    let root = folder(&mut store, "root");
    store.pin(root);

    for step in 0..3000 {
        let folders: Vec<Id<Folder>> = store.all::<Folder>().map(|(id, _)| id).collect();
        let items: Vec<Id<Item>> = store.all::<Item>().map(|(id, _)| id).collect();
        let f = folders[rng.below(folders.len())];
        let g = folders[rng.below(folders.len())];
        let i = items.get(rng.below(items.len())).copied();
        let j = items.get(rng.below(items.len())).copied();
        match rng.below(16) {
            0 => {
                let new = folder(&mut store, &format!("f{step}"));
                store.edit(f).children.push(new);
            }
            1 => {
                let new = item(&mut store, &format!("i{step}"));
                store.edit(f).items.push(new);
            }
            2 => store.edit(f).parent = Some(g),
            3 => store.edit(f).children.push(g),
            4 => {
                let mut f = store.edit(f);
                if !f.children.is_empty() {
                    let n = rng.below(f.children.len());
                    f.children.remove(n);
                }
            }
            5 => {
                let mut f = store.edit(f);
                if !f.items.is_empty() {
                    let n = rng.below(f.items.len());
                    f.items.remove(n);
                }
            }
            6 => {
                store.pin(f);
            }
            7 if f != root => {
                store.unpin(f);
            }
            8 => {
                if let Some(i) = i {
                    store.edit(i).folder = Some(f);
                }
            }
            9 => {
                if let (Some(i), Some(j)) = (i, j) {
                    store.edit(i).link = Some(j);
                    store.edit(i).marks.insert(j, step as u8);
                }
            }
            10 => {
                if let Some(i) = i {
                    store.edit(i).seen.push(f);
                }
            }
            11 => {
                if let Some(i) = i {
                    store.edit(f).owners.insert(i, format!("o{step}"));
                    store.edit(g).by_name.insert(format!("n{}", rng.below(3)), i);
                }
            }
            12 => store.edit(f).owners.clear(),
            13 => {
                if let Some(i) = i {
                    store.edit(i).vote = Vote { by: Some(g).filter(|_| rng.below(2) == 0) };
                }
            }
            _ => {
                store.collect();
                for id in all_ids(&store) {
                    let exists = store.contains(Id::<Folder>::from_raw(id)) || store.contains(Id::<Item>::from_raw(id));
                    assert!(exists, "step {step}: id {id} dangles");
                }
                assert_eq!(store.collect(), 0, "step {step}");
            }
        }
    }
    let log = log_since(&store, 0);
    // Adjacent removals from `seen` are logged as one splice.
    for kind in ["delete Folder", "delete Item", "set link=None", "seen.remove", "seen.splice", "marks.remove"] {
        let n = log.iter().filter(|e| e.contains(kind) && (!e.contains("splice") || e.ends_with(", [])"))).count();
        assert!(n > 0, "no `{kind}` events");
    }
    assert_eq!(store.verify(), Ok(()));
}
