use std::sync::{Arc, Mutex};

use kbd_handoff_core::crypto::{SecureReader, SecureWriter};
use kbd_handoff_core::input::KeyEvent;
use tokio::net::TcpListener;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

use super::*;

type Events = Arc<Mutex<Vec<(u16, KeyValue)>>>;

#[derive(Clone, Default)]
struct RecSink(Events);

impl RecSink {
    fn events(&self) -> Vec<(u16, KeyValue)> {
        self.0.lock().unwrap().clone()
    }
}

impl KeySink for RecSink {
    fn inject(&mut self, code: u16, value: KeyValue) -> anyhow::Result<()> {
        self.0.lock().unwrap().push((code, value));
        Ok(())
    }
}

fn psk() -> Psk {
    Psk::from_bytes([0x5a; 32])
}

fn opts(addr: std::net::SocketAddr) -> ClientOptions {
    ClientOptions {
        server: addr.to_string(),
        psk: psk(),
        heartbeat_interval: Duration::from_millis(50),
        heartbeat_timeout: Duration::from_millis(250),
        reconnect_min: Duration::from_millis(10),
        reconnect_max: Duration::from_millis(40),
    }
}

async fn listen() -> (TcpListener, std::net::SocketAddr) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    (l, addr)
}

/// Accept one connection on the fake sender and check the client's `Hello`.
async fn accept_one(
    l: &TcpListener,
    role: Role,
) -> (SecureReader<OwnedReadHalf>, SecureWriter<OwnedWriteHalf>) {
    let (s, _) = l.accept().await.unwrap();
    let (r, w) = s.into_split();
    let (mut r, w) = crypto::accept(r, w, &psk()).await.unwrap();
    assert_eq!(r.recv().await.unwrap(), Msg::Hello { role });
    (r, w)
}

fn key(code: u16, value: KeyValue) -> Msg {
    Msg::Key(KeyEvent { code, value })
}

#[test]
fn held_keys_release_all() {
    let rec = RecSink::default();
    let mut held = HeldKeys::new(rec.clone());
    held.inject(30, KeyValue::Down).unwrap();
    held.inject(48, KeyValue::Down).unwrap();
    held.inject(48, KeyValue::Repeat).unwrap();
    held.inject(29, KeyValue::Repeat).unwrap();
    held.inject(30, KeyValue::Up).unwrap();
    assert_eq!(held.held().collect::<Vec<_>>(), vec![29, 48]);

    held.release_all().unwrap();
    assert!(held.is_empty());
    let ev = rec.events();
    assert_eq!(
        ev[5..],
        [(29, KeyValue::Up), (48, KeyValue::Up)],
        "release_all must send Up for each held key: {ev:?}"
    );

    // Nothing is held any more, so a second call and the drop send nothing.
    held.release_all().unwrap();
    drop(held);
    assert_eq!(rec.events().len(), 7);
}

#[test]
fn held_keys_release_on_drop() {
    let rec = RecSink::default();
    {
        let mut held = HeldKeys::new(rec.clone());
        held.inject(42, KeyValue::Down).unwrap();
    }
    assert_eq!(rec.events(), [(42, KeyValue::Down), (42, KeyValue::Up)]);
}

#[test]
fn held_keys_release_continues_after_error() {
    struct Failing(Vec<u16>);
    impl KeySink for Failing {
        fn inject(&mut self, code: u16, value: KeyValue) -> anyhow::Result<()> {
            if value == KeyValue::Up {
                self.0.push(code);
                if code == 1 {
                    bail!("boom");
                }
            }
            Ok(())
        }
    }
    let mut held = HeldKeys::new(Failing(Vec::new()));
    for code in [1, 2, 3] {
        held.inject(code, KeyValue::Down).unwrap();
    }
    assert!(held.release_all().is_err());
    assert!(held.is_empty());
    assert_eq!(held.sink().0, [1, 2, 3]);
}

#[test]
fn backoff_doubles_and_saturates() {
    let max = Duration::from_secs(10);
    assert_eq!(
        next_delay(Duration::from_secs(1), max),
        Duration::from_secs(2)
    );
    assert_eq!(next_delay(Duration::from_secs(8), max), max);
    assert_eq!(next_delay(Duration::MAX, max), max);
}

#[tokio::test]
async fn session_injects_and_releases() {
    let (l, addr) = listen().await;
    let server = tokio::spawn(async move {
        let (_r, mut w) = accept_one(&l, Role::Session).await;
        w.send(&Msg::Grabbed).await.unwrap();
        w.send(&key(30, KeyValue::Down)).await.unwrap();
        w.send(&key(30, KeyValue::Repeat)).await.unwrap();
        w.send(&Msg::Released).await.unwrap();
        w.send(&key(48, KeyValue::Down)).await.unwrap();
        // Dropping both halves closes the connection while 48 is held.
    });
    let mut rec = RecSink::default();
    let res = run_session(&opts(addr), &mut rec).await;
    server.await.unwrap();
    assert!(res.is_err(), "a disconnect must be reported as an error");
    assert_eq!(
        rec.events(),
        [
            (30, KeyValue::Down),
            (30, KeyValue::Repeat),
            (30, KeyValue::Up),
            (48, KeyValue::Down),
            (48, KeyValue::Up),
        ]
    );
}

#[tokio::test]
async fn heartbeat_timeout_releases_and_client_pings() {
    let (l, addr) = listen().await;
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (mut r, mut w) = accept_one(&l, Role::Session).await;
        w.send(&key(42, KeyValue::Down)).await.unwrap();
        // Stay connected but silent; the client must keep pinging.
        assert_eq!(r.recv().await.unwrap(), Msg::Ping);
        assert_eq!(r.recv().await.unwrap(), Msg::Ping);
        let _ = done_rx.await;
    });
    let o = opts(addr);
    let mut rec = RecSink::default();
    let start = Instant::now();
    let res = tokio::time::timeout(Duration::from_secs(5), run_session(&o, &mut rec))
        .await
        .expect("heartbeat timeout did not fire");
    let err = format!("{:#}", res.unwrap_err());
    assert!(err.contains("no message"), "{err}");
    assert!(start.elapsed() >= o.heartbeat_timeout);
    assert_eq!(rec.events(), [(42, KeyValue::Down), (42, KeyValue::Up)]);
    let _ = done_tx.send(());
    server.await.unwrap();
}

#[tokio::test]
async fn session_survives_while_pinged() {
    let (l, addr) = listen().await;
    let server = tokio::spawn(async move {
        let (_r, mut w) = accept_one(&l, Role::Session).await;
        // Keep the session alive for longer than the heartbeat timeout.
        for _ in 0..10 {
            w.send(&Msg::Ping).await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        w.send(&key(2, KeyValue::Down)).await.unwrap();
    });
    let mut rec = RecSink::default();
    assert!(run_session(&opts(addr), &mut rec).await.is_err());
    server.await.unwrap();
    assert_eq!(rec.events(), [(2, KeyValue::Down), (2, KeyValue::Up)]);
}

#[tokio::test]
async fn wrong_psk_fails() {
    let (l, addr) = listen().await;
    let server = tokio::spawn(async move {
        let (s, _) = l.accept().await.unwrap();
        let (r, w) = s.into_split();
        assert!(
            crypto::accept(r, w, &Psk::from_bytes([1; 32]))
                .await
                .is_err()
        );
    });
    let mut rec = RecSink::default();
    assert!(run_session(&opts(addr), &mut rec).await.is_err());
    server.await.unwrap();
    assert!(rec.events().is_empty());
}

#[tokio::test]
async fn toggle_returns_server_outcome() {
    for outcome in [
        ToggleOutcome::Grabbed,
        ToggleOutcome::Released,
        ToggleOutcome::NoSession,
        ToggleOutcome::Failed,
    ] {
        let (l, addr) = listen().await;
        let server = tokio::spawn(async move {
            let (mut r, mut w) = accept_one(&l, Role::Toggle).await;
            assert_eq!(r.recv().await.unwrap(), Msg::ToggleRequest);
            // Simulate a slow grab: the client must keep pinging meanwhile.
            w.send(&Msg::Ping).await.unwrap();
            assert_eq!(r.recv().await.unwrap(), Msg::Ping);
            w.send(&Msg::ToggleResult(outcome)).await.unwrap();
        });
        assert_eq!(toggle(&opts(addr)).await.unwrap(), outcome);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn toggle_fails_when_connection_closes() {
    let (l, addr) = listen().await;
    let server = tokio::spawn(async move {
        let (mut r, _w) = accept_one(&l, Role::Toggle).await;
        assert_eq!(r.recv().await.unwrap(), Msg::ToggleRequest);
    });
    assert!(toggle(&opts(addr)).await.is_err());
    server.await.unwrap();
}

#[tokio::test]
async fn run_forever_reconnects() {
    let (l, addr) = listen().await;
    let server = async move {
        for code in [10u16, 11, 12] {
            let (_r, mut w) = accept_one(&l, Role::Session).await;
            w.send(&key(code, KeyValue::Down)).await.unwrap();
        }
    };
    let o = opts(addr);
    let mut rec = RecSink::default();
    let rec2 = rec.clone();
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            _ = run_forever(&o, &mut rec) => unreachable!(),
            () = server => {}
        }
    })
    .await
    .expect("client did not reconnect");
    // The client reconnects only after a session has ended and released its
    // keys, so the first two sessions are complete by the third accept.
    let ev = rec2.events();
    assert!(
        ev.starts_with(&[
            (10, KeyValue::Down),
            (10, KeyValue::Up),
            (11, KeyValue::Down),
            (11, KeyValue::Up),
        ]),
        "{ev:?}"
    );
}

#[test]
fn futures_are_send() {
    fn assert_send<T: Send>(_: &T) {}
    let o = ClientOptions::new("192.0.2.10:47820", psk());
    let mut sink = LogSink;
    let f = run_session(&o, &mut sink);
    assert_send(&f);
    drop(f);
    let f = toggle(&o);
    assert_send(&f);
    drop(f);
    let f = run_forever(&o, &mut sink);
    assert_send(&f);
}

#[test]
fn config_parses() {
    let text = format!(
        "server = \"192.0.2.10:47820\"\npsk = \"{}\"\n",
        "ab".repeat(32)
    );
    let cfg: ReceiverConfig = toml_from_str(&text);
    let o = cfg.into_options();
    assert_eq!(o.server, "192.0.2.10:47820");
    assert_eq!(o.psk, Psk::from_bytes([0xab; 32]));
    assert_eq!(o.heartbeat_interval, kbd_handoff_core::HEARTBEAT_INTERVAL);
}

fn toml_from_str(text: &str) -> ReceiverConfig {
    let dir =
        std::env::temp_dir().join(format!("kbd-handoff-receiver-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("receiver.toml");
    std::fs::write(&path, text).unwrap();
    let cfg = kbd_handoff_core::config::load(&path).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    cfg
}

#[tokio::test]
async fn run_until_releases_held_keys_on_shutdown() {
    let (l, addr) = listen().await;
    let o = opts(addr);
    let mut rec = RecSink::default();
    let rec2 = rec.clone();
    let server = tokio::spawn(async move {
        let (_r, mut w) = accept_one(&l, Role::Session).await;
        w.send(&key(56, KeyValue::Down)).await.unwrap();
        // Keep the connection (and the held key) alive until shutdown.
        std::future::pending::<()>().await;
    });
    let shutdown = async {
        let end = Instant::now() + Duration::from_secs(5);
        while rec2.events() != [(56, KeyValue::Down)] {
            assert!(Instant::now() < end, "key was not injected");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        "stop"
    };
    let reason = tokio::time::timeout(Duration::from_secs(10), run_until(&o, &mut rec, shutdown))
        .await
        .expect("run_until did not return");
    assert_eq!(reason, "stop");
    assert_eq!(
        rec2.events(),
        [(56, KeyValue::Down), (56, KeyValue::Up)],
        "held keys must be released before run_until returns"
    );
    server.abort();
}

/// Fake sender side of an on-demand session: accept, send the registration
/// ping, and expect the session's own `ToggleRequest` (pings may precede it).
async fn accept_on_demand(
    l: &TcpListener,
) -> (SecureReader<OwnedReadHalf>, SecureWriter<OwnedWriteHalf>) {
    let (mut r, mut w) = accept_one(l, Role::Session).await;
    w.send(&Msg::Ping).await.unwrap();
    loop {
        match r.recv().await.unwrap() {
            Msg::Ping => continue,
            Msg::ToggleRequest => return (r, w),
            other => panic!("expected ToggleRequest, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn on_demand_requests_after_registration_and_ends_on_release() {
    let (l, addr) = listen().await;
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel::<Vec<Msg>>();
    let server = tokio::spawn(async move {
        let (mut r, mut w) = accept_one(&l, Role::Session).await;
        // Before registration, the client must not request a toggle.
        tokio::time::sleep(Duration::from_millis(120)).await;
        let mut before = Vec::new();
        while let Ok(Ok(m)) = tokio::time::timeout(Duration::from_millis(1), r.recv()).await {
            before.push(m);
        }
        let _ = seen_tx.send(before);
        // Register the session; the request must follow.
        w.send(&Msg::Ping).await.unwrap();
        loop {
            match r.recv().await.unwrap() {
                Msg::Ping => continue,
                Msg::ToggleRequest => break,
                other => panic!("expected ToggleRequest, got {other:?}"),
            }
        }
        w.send(&Msg::Grabbed).await.unwrap();
        w.send(&Msg::ToggleResult(ToggleOutcome::Grabbed))
            .await
            .unwrap();
        w.send(&key(29, KeyValue::Down)).await.unwrap();
        w.send(&key(30, KeyValue::Down)).await.unwrap();
        w.send(&key(30, KeyValue::Up)).await.unwrap();
        w.send(&Msg::Released).await.unwrap();
        // Keep the connection open: the client must end on Released alone.
        std::future::pending::<()>().await;
    });
    let mut rec = RecSink::default();
    let res = tokio::time::timeout(Duration::from_secs(5), run_on_demand(&opts(addr), &mut rec))
        .await
        .expect("run_on_demand did not return after Released");
    res.unwrap();
    let before = seen_rx.await.unwrap();
    assert!(
        before.iter().all(|m| *m == Msg::Ping),
        "only pings may be sent before registration: {before:?}"
    );
    assert_eq!(
        rec.events(),
        [
            (29, KeyValue::Down),
            (30, KeyValue::Down),
            (30, KeyValue::Up),
            (29, KeyValue::Up),
        ],
        "held keys must be released on Released"
    );
    server.abort();
}

#[tokio::test]
async fn on_demand_not_grabbed_is_an_error() {
    for outcome in [ToggleOutcome::Failed, ToggleOutcome::NoSession] {
        let (l, addr) = listen().await;
        let server = tokio::spawn(async move {
            let (_r, mut w) = accept_on_demand(&l).await;
            w.send(&Msg::ToggleResult(outcome)).await.unwrap();
            std::future::pending::<()>().await;
        });
        let mut rec = RecSink::default();
        let res =
            tokio::time::timeout(Duration::from_secs(5), run_on_demand(&opts(addr), &mut rec))
                .await
                .expect("run_on_demand did not return");
        let err = res.unwrap_err();
        assert_eq!(err.downcast_ref::<NotGrabbed>(), Some(&NotGrabbed(outcome)));
        assert!(rec.events().is_empty());
        server.abort();
    }
}

#[tokio::test]
async fn on_demand_disconnect_releases_held_keys() {
    let (l, addr) = listen().await;
    let server = tokio::spawn(async move {
        let (_r, mut w) = accept_on_demand(&l).await;
        w.send(&Msg::ToggleResult(ToggleOutcome::Grabbed))
            .await
            .unwrap();
        w.send(&key(42, KeyValue::Down)).await.unwrap();
        // Dropping both halves closes the connection while 42 is held.
    });
    let mut rec = RecSink::default();
    let res = tokio::time::timeout(Duration::from_secs(5), run_on_demand(&opts(addr), &mut rec))
        .await
        .expect("run_on_demand did not return after the disconnect");
    server.await.unwrap();
    let err = res.unwrap_err();
    assert!(err.downcast_ref::<NotGrabbed>().is_none(), "{err:#}");
    assert_eq!(rec.events(), [(42, KeyValue::Down), (42, KeyValue::Up)]);
}

#[tokio::test]
async fn on_demand_heartbeat_timeout_releases_held_keys() {
    let (l, addr) = listen().await;
    let server = tokio::spawn(async move {
        let (r, mut w) = accept_on_demand(&l).await;
        w.send(&Msg::ToggleResult(ToggleOutcome::Grabbed))
            .await
            .unwrap();
        w.send(&key(56, KeyValue::Down)).await.unwrap();
        // Stay connected but silent.
        let _keep = r;
        std::future::pending::<()>().await;
    });
    let o = opts(addr);
    let mut rec = RecSink::default();
    let res = tokio::time::timeout(Duration::from_secs(5), run_on_demand(&o, &mut rec))
        .await
        .expect("heartbeat timeout did not fire");
    let err = format!("{:#}", res.unwrap_err());
    assert!(err.contains("no message"), "{err}");
    assert_eq!(rec.events(), [(56, KeyValue::Down), (56, KeyValue::Up)]);
    server.abort();
}

#[tokio::test]
async fn on_demand_cancel_releases_held_keys() {
    let (l, addr) = listen().await;
    let server = tokio::spawn(async move {
        let (_r, mut w) = accept_on_demand(&l).await;
        w.send(&Msg::ToggleResult(ToggleOutcome::Grabbed))
            .await
            .unwrap();
        w.send(&key(97, KeyValue::Down)).await.unwrap();
        loop {
            w.send(&Msg::Ping).await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    let o = opts(addr);
    let mut rec = RecSink::default();
    let rec2 = rec.clone();
    let shutdown = async {
        let end = Instant::now() + Duration::from_secs(5);
        while rec2.events() != [(97, KeyValue::Down)] {
            assert!(Instant::now() < end, "key was not injected");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    tokio::select! {
        res = run_on_demand(&o, &mut rec) => panic!("session ended early: {res:?}"),
        () = shutdown => {}
    }
    assert_eq!(rec2.events(), [(97, KeyValue::Down), (97, KeyValue::Up)]);
    server.abort();
}

#[test]
fn on_demand_future_is_send() {
    fn assert_send<T: Send>(_: &T) {}
    let o = ClientOptions::new("192.0.2.10:47820", psk());
    let mut sink = LogSink;
    let f = run_on_demand(&o, &mut sink);
    assert_send(&f);
}
