mod common;
use common::Fixture;
use memz::{
    auth::{Scope, Visibility},
    catalog::Catalog,
    state::StateRoot,
};
#[test]
fn grants_bind_identity_hide_existence_and_revoke_persistently() {
    let f = Fixture::new("auth");
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    let mut c = Catalog::open(state.clone()).unwrap();
    let a = f.root.join("a");
    let b = f.root.join("b");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    let aid = c.register(&a).unwrap();
    let bid = c.register(&b).unwrap();
    c.bind_session("alice", "session-a").unwrap();
    c.bind_session("bob", "session-b").unwrap();
    assert!(c.bind_session("bob", "session-a").is_err());
    c.grant("alice", &aid, true).unwrap();
    let cap = c.capability("alice", "session-a").unwrap();
    assert_eq!(cap.principal(), "alice");
    let private = Scope::session(&aid, "session-a");
    c.authorize(&cap, &private, true).unwrap();
    c.authorize(&cap, &Scope::project(&aid), false).unwrap();
    let denied = c.authorize(&cap, &Scope::project(&bid), false).unwrap_err();
    for scope in [
        Scope::project("unknown"),
        Scope::session(&aid, "session-b"),
        Scope::session(&aid, "fake"),
    ] {
        let error = c.authorize(&cap, &scope, false).unwrap_err();
        assert_eq!(
            (error.code, error.message),
            (denied.code, denied.message.clone())
        );
    }
    assert!(c.capability("alice", "session-b").is_err());
    assert!(c.resolve_authorized(&cap, &b, None, false).is_err());
    let nested = a.join("nested");
    std::fs::create_dir(&nested).unwrap();
    c.register(&nested).unwrap();
    assert!(c.resolve_authorized(&cap, &nested, None, false).is_err());
    assert_eq!(
        c.resolve_authorized(&cap, &nested, Some(&aid), false)
            .unwrap(),
        aid
    );
    c.grant("alice", &bid, false).unwrap();
    assert!(c.authorize(&cap, &private, false).is_err());
    let fresh = c.capability("alice", "session-a").unwrap();
    c.authorize(&fresh, &Scope::project(&bid), false).unwrap();
    assert!(c.authorize(&fresh, &Scope::project(&bid), true).is_err());
    c.revoke("alice", &aid).unwrap();
    drop(c);
    let c = Catalog::open(state).unwrap();
    assert!(c.authorize(&fresh, &Scope::project(&bid), false).is_err());
    let fresh = c.capability("alice", "session-a").unwrap();
    assert!(c.authorize(&fresh, &private, false).is_err());
    c.authorize(&fresh, &Scope::project(&bid), false).unwrap();
    assert!(matches!(private.visibility, Visibility::Session(_)));
}
#[test]
fn derivatives_never_broaden_or_mix_visibility() {
    let p = Scope::project("a");
    let s = Scope::session("a", "one");
    assert!(Scope::derive(&[p.clone(), p.clone()], &p).is_ok());
    for inputs in [
        vec![],
        vec![p.clone(), s.clone()],
        vec![Scope::project("b")],
        vec![Scope::session("a", "two")],
    ] {
        assert!(Scope::derive(&inputs, &s).is_err());
    }
    assert!(Scope::derive(std::slice::from_ref(&s), &p).is_err());
    assert!(Scope::derive(std::slice::from_ref(&s), &s).is_ok());
}
