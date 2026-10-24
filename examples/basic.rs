use replayable::{model, List, Map, Store};

model! {
    pub World {
        User {
            name: String,
            age: u8,
            parent: User,          // a reference to another User
            tags: List<String>,    // a list: the whole Vec API, every change logged
        }
        Group {
            title: String,
            members: List<User>,       // a list of references
            roles: Map<User, String>,  // a map with references as keys
        }
    }
}

fn main() {
    let mut store = Store::<World>::new();

    // Create with a plain struct literal
    let sune = store.create(User { name: "Sune".into(), age: 14, ..Default::default() });
    let bertil = store.create(User { name: "Bertil".into(), age: 42, ..Default::default() });

    // Change one field
    store.edit(sune).parent = Some(bertil);

    // Change several fields, just like on a plain &mut User
    {
        let mut u = store.edit(sune);
        u.age += 1;
        u.tags.push("chess".into());
        u.tags.push("football".into());
    }
    store.edit(sune).tags.retain(|t| t != "chess");

    // Read other entities while editing
    store.update(sune, |u, store| {
        u.name = format!("{} {}son", u.name, store[bertil].name);
    });

    let club = store.create(Group { title: "The Club".into(), ..Default::default() });
    store.edit(club).members.extend([sune, bertil]);
    store.update(club, |g, store| {
        g.members.retain(|&m| store[m].age >= 18);
    });
    store.edit(club).roles.insert(bertil, "chair".into());

    // Pin what should stay. `collect` deletes everything that nothing
    // pinned refers to; Bertil stays as Sune's parent and the club's chair.
    store.pin(sune);
    store.pin(club);
    let guest = store.create(User { name: "Guest".into(), ..Default::default() });
    assert_eq!(store.collect(), 1);
    assert!(!store.contains(guest));

    // Read: store[id] gives a plain &User
    let parent = store[sune].parent.unwrap();
    println!("{} ({} years) has parent {}", store[sune].name, store[sune].age, store[parent].name);
    for &m in &store[club].members {
        println!("{} is in {}", store[m].name, store[club].title);
    }
    for (&m, role) in &store[club].roles {
        println!("{} is {role}", store[m].name);
    }

    println!("\nEvent log:");
    for (i, (at, e)) in store.timestamps().iter().zip(store.events()).enumerate() {
        println!("{:>3}. {at} {e}", i + 1);
    }

    // Save, load again and compare
    let loaded = Store::<World>::from_json(&store.to_json().unwrap()).unwrap();
    assert_eq!(loaded[sune], store[sune]);
    assert_eq!(loaded[club], store[club]);
    println!("\nLoaded from JSON: identical ✔");
}
