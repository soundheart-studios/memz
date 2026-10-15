mod common;
use common::Fixture;
use memz::{
    daemon::{Binding, Limits, Server, request},
    state::StateRoot,
};
use serde_json::json;
use std::{
    os::unix::fs::PermissionsExt,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
#[test]
fn operational_error_codes_and_messages_survive_the_socket() {
    let f = Fixture::new("daemon-errors");
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    let codes = [
        "export_limit",
        "storage",
        "cancelled",
        "semantic_deadline",
        "invalid_query",
        "quota_exceeded",
        "denied",
        "busy",
        "future_unknown",
    ];
    let server = Server::start(
        &state,
        vec![Binding {
            token: "t".into(),
            principal: "p".into(),
            session: "s".into(),
        }],
        Limits::default(),
        move |_, value| {
            Err(memz::Error::new(
                codes[value.as_u64().unwrap() as usize],
                "specific operational failure",
            ))
        },
    )
    .unwrap();
    for (index, code) in codes.iter().enumerate() {
        let error = request(server.socket(), "t", 1, json!(index)).unwrap_err();
        assert_eq!(
            error.code,
            if *code == "future_unknown" {
                "invalid_request"
            } else {
                code
            }
        );
        assert_eq!(error.message, "specific operational failure");
    }
}
#[test]
fn private_daemon_locks_authenticates_versions_and_restarts() {
    let f = Fixture::new("daemon");
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    let binding = Binding {
        token: "private-token".into(),
        principal: "trusted".into(),
        session: "session".into(),
    };
    let server = Server::start(
        &state,
        vec![binding.clone()],
        Limits::default(),
        |binding, value| Ok(json!({"principal":binding.principal,"value":value})),
    )
    .unwrap();
    assert_eq!(
        std::fs::metadata(server.socket())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(
        Server::start(&state, vec![binding.clone()], Limits::default(), |_, v| Ok(
            v
        ))
        .is_err()
    );
    assert_eq!(
        request(server.socket(), "private-token", 1, json!(42)).unwrap()["principal"],
        "trusted"
    );
    assert_eq!(
        request(server.socket(), "wrong", 1, json!(42))
            .unwrap_err()
            .code,
        "denied"
    );
    assert_eq!(
        request(server.socket(), "private-token", 99, json!(42))
            .unwrap_err()
            .code,
        "protocol_mismatch"
    );
    drop(server);
    let stale = std::os::unix::net::UnixListener::bind(state.path.join("run/memz.sock")).unwrap();
    drop(stale);
    let server = Server::start(&state, vec![binding], Limits::default(), |_, v| Ok(v)).unwrap();
    assert_eq!(
        request(server.socket(), "private-token", 1, json!(true)).unwrap(),
        true
    );
}
#[test]
fn bounded_workers_release_cancelled_connections() {
    let f = Fixture::new("bounds");
    let state = StateRoot::open(&f.root.join("state")).unwrap();
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let a = active.clone();
    let p = peak.clone();
    let server = Server::start(
        &state,
        vec![Binding {
            token: "t".into(),
            principal: "p".into(),
            session: "s".into(),
        }],
        Limits {
            workers: 2,
            queue: 2,
            max_request_bytes: 1024,
            timeout_ms: 100,
        },
        move |_, v| {
            let current = a.fetch_add(1, Ordering::SeqCst) + 1;
            p.fetch_max(current, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(25));
            a.fetch_sub(1, Ordering::SeqCst);
            Ok(v)
        },
    )
    .unwrap();
    std::thread::scope(|scope| {
        for _ in 0..20 {
            let path = server.socket();
            scope.spawn(move || {
                let _ = request(path, "t", 1, json!(1));
            });
        }
    });
    assert!(peak.load(Ordering::SeqCst) <= 2);
    for _ in 0..4 {
        drop(std::os::unix::net::UnixStream::connect(server.socket()).unwrap());
    }
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert_eq!(
        request(server.socket(), "t", 1, json!("alive")).unwrap(),
        "alive"
    );
    assert!(request(server.socket(), "t", 1, json!("x".repeat(2048))).is_err());
}
