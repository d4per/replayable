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
    let sam = store.create(User { name: "Sam".into(), age: 14, ..Default::default() });
    let tom = store.create(User { name: "Tom".into(), age: 42, ..Default::default() });

    // Change one field
    store.edit(sam).parent = Some(tom);

    // Change several fields, just like on a plain &mut User
    {
        let mut u = store.edit(sam);
        u.age += 1;
        u.tags.push("chess".into());
        u.tags.push("football".into());
    }
    store.edit(sam).tags.retain(|t| t != "chess");

    // Read other entities while editing
    store.update(sam, |u, store| {
        u.name = format!("{} {}son", u.name, store[tom].name);
    });

    let club = store.create(Group { title: "The Club".into(), ..Default::default() });
    store.edit(club).members.extend([sam, tom]);
    store.update(club, |g, store| {
        g.members.retain(|&m| store[m].age >= 18);
    });
    store.edit(club).roles.insert(tom, "chair".into());

    // Pin what should stay. `collect` deletes everything that nothing
    // pinned refers to; Tom stays as Sam's parent and the club's chair.
    store.pin(sam);
    store.pin(club);
    let guest = store.create(User { name: "Guest".into(), ..Default::default() });
    assert_eq!(store.collect(), 1);
    assert!(!store.contains(guest));

    // Read: store[id] gives a plain &User
    let parent = store[sam].parent.unwrap();
    println!("{} ({} years) has parent {}", store[sam].name, store[sam].age, store[parent].name);
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
    assert_eq!(loaded[sam], store[sam]);
    assert_eq!(loaded[club], store[club]);
    println!("\nLoaded from JSON: identical ✔");
}
