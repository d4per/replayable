//! Logs written by one version of a model, read by a later one.

use replayable::{Error, List, Store};

mod v1 {
    use replayable::List;

    replayable::model! {
        pub World {
            User { name: String, age: u8, nick: Option<String>, tags: List<String> }
            Human { title: String }
            Team { title: String }
        }
    }
}

mod v2 {
    use replayable::List;

    replayable::model! {
        pub World {
            #![removed(Team)]
            User {
                #![removed(age)]
                name: String,
                level: u8 = 1,
                nickname (was nick): Option<String>,
                tags: List<String>,
                score: u32,
            }
            Person (was Human) { title: String }
        }
    }
}

/// Like v2, with another default for `level`.
mod v3 {
    use replayable::List;

    replayable::model! {
        pub World {
            #![removed(Team)]
            User {
                #![removed(age)]
                name: String,
                level: u8 = 5,
                nickname (was nick): Option<String>,
                tags: List<String>,
                score: u32,
            }
            Person (was Human) { title: String }
        }
    }
}

/// v1, but `age` was removed without saying so.
mod undeclared {
    use replayable::List;

    replayable::model! {
        pub World {
            #![removed(Team)]
            User { name: String, nick: Option<String>, tags: List<String> }
            Human { title: String }
        }
    }
}

/// v1, but `Team` was removed without saying so.
mod undeclared_entity {
    use replayable::List;

    replayable::model! {
        pub World {
            User { name: String, age: u8, nick: Option<String>, tags: List<String> }
            Human { title: String }
        }
    }
}

/// v1, but `age` changed to a type its old values don't fit.
mod changed_type {
    use replayable::List;

    replayable::model! {
        pub World {
            #![removed(Team)]
            User { name: String, age: String, nick: Option<String>, tags: List<String> }
            Human { title: String }
        }
    }
}

/// v1, but with wider integer types, which old values still fit.
mod widened {
    use replayable::List;

    replayable::model! {
        pub World {
            #![removed(Team)]
            User { name: String, age: u64, nick: Option<String>, tags: List<String> }
            Human { title: String }
        }
    }
}

/// A log written with v1: two users, a human and a team, with some edits.
fn v1_log() -> String {
    v1_store().to_json().unwrap()
}

/// The same log, in the binary format.
fn v1_binary() -> Vec<u8> {
    v1_store().to_binary().unwrap()
}

fn v1_store() -> Store<v1::World> {
    use v1::*;
    let mut store = Store::<World>::new();
    let a = store.create(User { name: "a".into(), age: 30, nick: Some("ace".into()), ..Default::default() });
    let b = store.create(User { name: "b".into(), ..Default::default() });
    store.edit(a).age = 31;
    store.edit(b).nick = Some("bee".into());
    store.edit(b).tags.push("t".into());
    let h = store.create(Human { title: "dr".into() });
    store.edit(h).title = "prof".into();
    let t = store.create(Team { title: "x".into() });
    store.edit(t).title = "y".into();
    store
}

#[test]
fn a_newer_model_reads_an_older_log() {
    use v2::*;
    let store = Store::<World>::from_json(&v1_log()).unwrap();
    let users: Vec<&User> = store.all::<User>().map(|(_, u)| u).collect();

    // Added fields get their default: `= 1` for level, 0 for score.
    assert_eq!(users[0].level, 1);
    assert_eq!(users[0].score, 0);
    // Renamed fields keep their values, from both create and update events.
    assert_eq!(users[0].nickname.as_deref(), Some("ace"));
    assert_eq!(users[1].nickname.as_deref(), Some("bee"));
    assert_eq!(users[1].tags.as_slice(), ["t"]);
    // A renamed entity type.
    let (_, person) = store.all::<Person>().next().unwrap();
    assert_eq!(person.title, "prof");

    // Events about `age` and `Team` are skipped and not kept in the log.
    let kinds: Vec<String> = store.events().iter().map(|e| e.to_string()).collect();
    assert!(kinds.iter().all(|e| !e.contains("age") && !e.contains("Team")), "{kinds:#?}");
    assert_eq!(store.events().len(), 6);
}

#[test]
fn ids_of_removed_entities_are_not_reused() {
    use v2::*;
    let mut store = Store::<World>::from_json(&v1_log()).unwrap();
    // The removed Team had id 4, the last one handed out by v1.
    let p = store.create(Person::default());
    assert_eq!(p.raw(), 5);
}

#[test]
fn a_rewritten_log_reads_back_the_same() {
    use v2::*;
    let mut store = Store::<World>::from_json(&v1_log()).unwrap();
    let c = store.create(User { name: "c".into(), level: 7, ..Default::default() });
    store.edit(c).score = 3;
    assert_eq!(store.verify(), Ok(()));

    let json = store.to_json().unwrap();
    assert!(json.contains(r#""nickname":"ace""#), "old names are written with their new name");
    let again = Store::<World>::from_json(&json).unwrap();
    assert_eq!(again[c], store[c]);
}

#[test]
fn changing_a_default_only_affects_entities_from_before_the_field() {
    // v2 appends to the v1 log, the way a log file is normally kept.
    let old = v1_log();
    let appended = {
        use v2::*;
        let mut store = Store::<World>::from_json(&old).unwrap();
        let saved = store.events().len();
        // Created with v2's default value. `create` logs it anyway.
        store.create(User { name: "new".into(), ..Default::default() });
        let mut out = Vec::new();
        store.write_json_since(saved, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    };

    use v3::*;
    let store = Store::<World>::from_json(&(old + &appended)).unwrap();
    let levels: Vec<(&str, u8)> = store.all::<User>().map(|(_, u)| (u.name.as_str(), u.level)).collect();
    assert_eq!(levels, [("a", 5), ("b", 5), ("new", 1)]);
}

#[test]
fn rewriting_a_log_fixes_the_defaults_it_was_read_with() {
    // Writing the whole log again writes every field of every create,
    // including the defaults filled in when it was read.
    let rewritten = {
        use v2::*;
        Store::<World>::from_json(&v1_log()).unwrap().to_json().unwrap()
    };
    use v3::*;
    let store = Store::<World>::from_json(&rewritten).unwrap();
    assert!(store.all::<User>().all(|(_, u)| u.level == 1));
}

#[test]
fn an_undeclared_removed_field_is_an_error() {
    let err = Store::<undeclared::World>::from_json(&v1_log()).unwrap_err();
    assert!(matches!(err, Error::Json { line: 1, .. }), "{err}");
    let message = err.to_string();
    assert!(message.contains("unknown field `age` in User"), "{message}");
    assert!(message.contains("#![removed(age)]"), "{message}");
}

#[test]
fn an_undeclared_removed_entity_is_an_error() {
    let err = Store::<undeclared_entity::World>::from_json(&v1_log()).unwrap_err();
    assert!(matches!(err, Error::Json { line: 8, .. }), "{err}");
    assert!(err.to_string().contains("unknown event `CreateTeam`"), "{err}");
}

#[test]
fn an_incompatible_type_change_is_an_error() {
    let err = Store::<changed_type::World>::from_json(&v1_log()).unwrap_err();
    assert!(matches!(err, Error::Json { line: 1, .. }), "{err}");
}

#[test]
fn a_widened_type_reads_old_values() {
    let store = Store::<widened::World>::from_json(&v1_log()).unwrap();
    let ages: Vec<u64> = store.all::<widened::User>().map(|(_, u)| u.age).collect();
    assert_eq!(ages, [31, 0]);
}

#[test]
fn missing_fields_get_their_default_inside_lists_too() {
    // A create event written by hand, with only some of the fields.
    use v2::*;
    let store =
        Store::<World>::from_json(r#"{"at":"2026-10-24T09:15:02.123Z","CreateUser":[1,{"name":"x"}]}"#).unwrap();
    let (_, user) = store.all::<User>().next().unwrap();
    assert_eq!((user.level, user.score, user.tags.clone()), (1, 0, List::new()));
}

#[test]
fn a_newer_model_reads_an_older_binary_log() {
    use v2::*;
    let v1 = v1_store();
    let mut file = v1.to_binary().unwrap();
    let from_json = Store::<World>::from_json(&v1.to_json().unwrap()).unwrap();
    let mut store = Store::<World>::from_binary(&file).unwrap();
    assert_eq!(store.events(), from_json.events());
    assert_eq!(store.timestamps(), from_json.timestamps());

    // Appending with the new model: new names after the old ones, which are
    // still defined in the file.
    let saved = store.events().len();
    let p = store.create(Person { title: "new".into() });
    assert_eq!(p.raw(), 5, "ids of removed entities are not reused");
    let c = store.create(User { name: "c".into(), level: 7, ..Default::default() });
    store.edit(c).nickname = Some("cee".into());
    store.write_binary_since(saved, &mut file).unwrap();

    let again = Store::<World>::from_binary(&file).unwrap();
    assert_eq!(again.events(), store.events());
    assert_eq!(again[c].nickname.as_deref(), Some("cee"));
    assert_eq!(again.verify(), Ok(()));
}

#[test]
fn an_undeclared_removal_in_a_binary_log_is_an_error() {
    let err = Store::<undeclared::World>::from_binary(&v1_binary()).unwrap_err();
    assert!(matches!(err, Error::Binary { offset: 5, .. }), "{err}");
    assert!(err.to_string().contains("unknown field `age` in User"), "{err}");
    let err = Store::<undeclared_entity::World>::from_binary(&v1_binary()).unwrap_err();
    assert!(err.to_string().contains("unknown event `CreateTeam`"), "{err}");
    assert!(Store::<widened::World>::from_binary(&v1_binary()).is_ok());
    assert!(Store::<changed_type::World>::from_binary(&v1_binary()).is_err());
}
