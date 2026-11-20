//! State machine tests with a fake key source over localhost TCP.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use kbd_handoff_core::crypto::SecureWriter;

use super::*;

const T: Duration = Duration::from_secs(5);

#[derive(Clone, Default)]
struct Fake {
    fail: Arc<AtomicBool>,
    grabs: Arc<AtomicUsize>,
    grabbed: Arc<AtomicBool>,
    keys: Arc<Mutex<Option<mpsc::UnboundedSender<anyhow::Result<KeyEvent>>>>>,
}

impl Fake {
    fn is_grabbed(&self) -> bool {
        self.grabbed.load(Ordering::SeqCst)
    }

    fn press(&self, code: u16, value: KeyValue) {
        self.keys
            .lock()
            .unwrap()
            .as_ref()
            .expect("not grabbed")
            .send(Ok(KeyEvent { code, value }))
            .unwrap();
    }

    fn fail_read(&self) {
        self.keys
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .send(Err(anyhow::anyhow!("unplugged")))
            .unwrap();
    }
}

struct FakeGrab {
    rx: mpsc::UnboundedReceiver<anyhow::Result<KeyEvent>>,
    grabbed: Arc<AtomicBool>,
}

impl Drop for FakeGrab {
    fn drop(&mut self) {
        self.grabbed.store(false, Ordering::SeqCst);
    }
}

impl KeySource for Fake {
    type Grab = FakeGrab;
    fn grab(
        &mut self,
        _timeout: Duration,
    ) -> impl Future<Output = anyhow::Result<FakeGrab>> + Send {
        let this = self.clone();
        async move {
            this.grabs.fetch_add(1, Ordering::SeqCst);
            if this.fail.load(Ordering::SeqCst) {
                bail!("fake grab failure");
            }
            let (tx, rx) = mpsc::unbounded_channel();
            *this.keys.lock().unwrap() = Some(tx);
            this.grabbed.store(true, Ordering::SeqCst);
            Ok(FakeGrab {
                rx,
                grabbed: this.grabbed.clone(),
            })
        }
    }
}

impl GrabbedKeyboard for FakeGrab {
    async fn next_event(&mut self) -> anyhow::Result<KeyEvent> {
        match self.rx.recv().await {
            Some(r) => r,
            None => pending().await,
        }
    }
}

fn psk() -> Psk {
    Psk::from_bytes([42u8; 32])
}

fn opts(chord: Vec<u16>) -> ServerOptions {
    ServerOptions {
        psk: psk(),
        emergency_chord: chord,
        heartbeat_interval: Duration::from_millis(100),
        heartbeat_timeout: Duration::from_secs(30),
        grab_timeout: Duration::from_secs(1),
    }
}

async fn start(fake: &Fake, opts: ServerOptions) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // `tokio::spawn` also proves that the `serve` future is `Send`.
    tokio::spawn(serve(listener, fake.clone(), opts));
    addr
}

struct Client {
    rx: crypto::ReaderChannel,
    w: SecureWriter<OwnedWriteHalf>,
}

impl Client {
    async fn connect(addr: SocketAddr, role: Role) -> Self {
        Self::connect_with(addr, role, &psk()).await.unwrap()
    }

    async fn connect_with(addr: SocketAddr, role: Role, psk: &Psk) -> Result<Self, crypto::Error> {
        let (r, w) = tokio::net::TcpStream::connect(addr)
            .await
            .unwrap()
            .into_split();
        let (r, mut w) = crypto::connect(r, w, psk).await?;
        w.send(&Msg::Hello { role }).await?;
        Ok(Self {
            rx: crypto::spawn_reader(r),
            w,
        })
    }

    /// Connect as a session and wait for the "registered" ping.
    async fn session(addr: SocketAddr) -> Self {
        let mut c = Self::connect(addr, Role::Session).await;
        assert_eq!(c.recv_raw().await, Some(Msg::Ping));
        c
    }

    async fn recv_raw(&mut self) -> Option<Msg> {
        match timeout(T, self.rx.recv()).await.expect("timed out") {
            Some(Ok(m)) => Some(m),
            _ => None,
        }
    }

    /// Next non-ping message; `None` on EOF or error.
    async fn recv(&mut self) -> Option<Msg> {
        loop {
            match self.recv_raw().await {
                Some(Msg::Ping) => continue,
                other => return other,
            }
        }
    }

    async fn send(&mut self, m: Msg) {
        self.w.send(&m).await.unwrap();
    }

    /// Assert that only pings arrive for `d`.
    async fn expect_quiet(&mut self, d: Duration) {
        let end = Instant::now() + d;
        while let Ok(m) = tokio::time::timeout_at(end, self.rx.recv()).await {
            match m {
                Some(Ok(Msg::Ping)) => {}
                other => panic!("unexpected {other:?}"),
            }
        }
    }
}

async fn toggle(addr: SocketAddr) -> ToggleOutcome {
    let mut c = Client::connect(addr, Role::Toggle).await;
    c.send(Msg::ToggleRequest).await;
    let res = match c.recv().await {
        Some(Msg::ToggleResult(o)) => o,
        other => panic!("expected ToggleResult, got {other:?}"),
    };
    assert_eq!(c.recv_raw().await, None, "toggle connection must be closed");
    res
}

fn key(code: u16, value: KeyValue) -> Msg {
    Msg::Key(KeyEvent { code, value })
}

async fn wait_until(mut f: impl FnMut() -> bool) {
    let end = Instant::now() + T;
    while !f() {
        assert!(Instant::now() < end, "condition not reached");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn toggle_without_session() {
    let fake = Fake::default();
    let addr = start(&fake, opts(vec![])).await;
    assert_eq!(toggle(addr).await, ToggleOutcome::NoSession);
    assert_eq!(
        fake.grabs.load(Ordering::SeqCst),
        0,
        "device must not be opened"
    );
}

#[tokio::test]
async fn toggle_on_and_off_forwards_keys() {
    let fake = Fake::default();
    let addr = start(&fake, opts(vec![29, 56, 14])).await;
    let mut s = Client::session(addr).await;

    assert_eq!(toggle(addr).await, ToggleOutcome::Grabbed);
    assert_eq!(s.recv().await, Some(Msg::Grabbed));
    assert!(fake.is_grabbed());

    fake.press(30, KeyValue::Down);
    fake.press(30, KeyValue::Repeat);
    fake.press(30, KeyValue::Up);
    assert_eq!(s.recv().await, Some(key(30, KeyValue::Down)));
    assert_eq!(s.recv().await, Some(key(30, KeyValue::Repeat)));
    assert_eq!(s.recv().await, Some(key(30, KeyValue::Up)));

    assert_eq!(toggle(addr).await, ToggleOutcome::Released);
    assert_eq!(s.recv().await, Some(Msg::Released));
    assert!(!fake.is_grabbed());

    // And again, so the state really went back to idle.
    assert_eq!(toggle(addr).await, ToggleOutcome::Grabbed);
    assert_eq!(s.recv().await, Some(Msg::Grabbed));
    assert_eq!(fake.grabs.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn session_can_toggle_itself() {
    let fake = Fake::default();
    let addr = start(&fake, opts(vec![])).await;
    let mut s = Client::session(addr).await;
    s.send(Msg::ToggleRequest).await;
    assert_eq!(s.recv().await, Some(Msg::Grabbed));
    assert_eq!(
        s.recv().await,
        Some(Msg::ToggleResult(ToggleOutcome::Grabbed))
    );
    s.send(Msg::ToggleRequest).await;
    assert_eq!(s.recv().await, Some(Msg::Released));
    assert_eq!(
        s.recv().await,
        Some(Msg::ToggleResult(ToggleOutcome::Released))
    );
    assert!(!fake.is_grabbed());
}

#[tokio::test]
async fn grab_failure_reports_failed_and_stays_idle() {
    let fake = Fake::default();
    fake.fail.store(true, Ordering::SeqCst);
    let addr = start(&fake, opts(vec![])).await;
    let mut s = Client::session(addr).await;
    assert_eq!(toggle(addr).await, ToggleOutcome::Failed);
    s.expect_quiet(Duration::from_millis(300)).await;
    assert!(!fake.is_grabbed());

    fake.fail.store(false, Ordering::SeqCst);
    assert_eq!(toggle(addr).await, ToggleOutcome::Grabbed);
    assert_eq!(s.recv().await, Some(Msg::Grabbed));
}

#[tokio::test]
async fn emergency_chord_releases() {
    let fake = Fake::default();
    let addr = start(&fake, opts(vec![29, 56])).await;
    let mut s = Client::session(addr).await;
    assert_eq!(toggle(addr).await, ToggleOutcome::Grabbed);
    assert_eq!(s.recv().await, Some(Msg::Grabbed));

    // A chord key pressed and released alone does not trigger.
    fake.press(56, KeyValue::Down);
    fake.press(56, KeyValue::Up);
    fake.press(29, KeyValue::Down);
    fake.press(29, KeyValue::Repeat);
    fake.press(56, KeyValue::Down);
    assert_eq!(s.recv().await, Some(key(56, KeyValue::Down)));
    assert_eq!(s.recv().await, Some(key(56, KeyValue::Up)));
    assert_eq!(s.recv().await, Some(key(29, KeyValue::Down)));
    assert_eq!(s.recv().await, Some(key(29, KeyValue::Repeat)));
    // The key completing the chord is not forwarded.
    assert_eq!(s.recv().await, Some(Msg::Released));
    wait_until(|| !fake.is_grabbed()).await;

    // Back to idle: the next toggle grabs again.
    assert_eq!(toggle(addr).await, ToggleOutcome::Grabbed);
}

#[tokio::test]
async fn empty_chord_is_disabled() {
    let fake = Fake::default();
    let addr = start(&fake, opts(vec![])).await;
    let mut s = Client::session(addr).await;
    assert_eq!(toggle(addr).await, ToggleOutcome::Grabbed);
    assert_eq!(s.recv().await, Some(Msg::Grabbed));
    fake.press(30, KeyValue::Down);
    assert_eq!(s.recv().await, Some(key(30, KeyValue::Down)));
    assert!(fake.is_grabbed());
}

#[tokio::test]
async fn session_disconnect_releases() {
    let fake = Fake::default();
    let addr = start(&fake, opts(vec![])).await;
    let mut s = Client::session(addr).await;
    assert_eq!(toggle(addr).await, ToggleOutcome::Grabbed);
    assert_eq!(s.recv().await, Some(Msg::Grabbed));
    drop(s);
    wait_until(|| !fake.is_grabbed()).await;
    assert_eq!(toggle(addr).await, ToggleOutcome::NoSession);
}

#[tokio::test]
async fn heartbeat_timeout_releases() {
    let fake = Fake::default();
    let mut o = opts(vec![]);
    o.heartbeat_timeout = Duration::from_secs(1);
    let addr = start(&fake, o).await;
    let mut s = Client::session(addr).await;
    s.send(Msg::ToggleRequest).await;
    assert_eq!(s.recv().await, Some(Msg::Grabbed));
    assert_eq!(
        s.recv().await,
        Some(Msg::ToggleResult(ToggleOutcome::Grabbed))
    );
    // The client sends nothing more, so the server must give up.
    wait_until(|| !fake.is_grabbed()).await;
    assert_eq!(s.recv().await, Some(Msg::Released));
    assert_eq!(s.recv().await, None);
}

#[tokio::test]
async fn new_session_replaces_old_and_releases() {
    let fake = Fake::default();
    let addr = start(&fake, opts(vec![])).await;
    let mut old = Client::session(addr).await;
    assert_eq!(toggle(addr).await, ToggleOutcome::Grabbed);
    assert_eq!(old.recv().await, Some(Msg::Grabbed));

    let mut new = Client::session(addr).await;
    assert!(!fake.is_grabbed());
    assert_eq!(old.recv().await, Some(Msg::Released));
    assert_eq!(old.recv().await, None, "old session must see EOF");

    assert_eq!(toggle(addr).await, ToggleOutcome::Grabbed);
    assert_eq!(new.recv().await, Some(Msg::Grabbed));
}

#[tokio::test]
async fn keyboard_error_releases() {
    let fake = Fake::default();
    let addr = start(&fake, opts(vec![])).await;
    let mut s = Client::session(addr).await;
    assert_eq!(toggle(addr).await, ToggleOutcome::Grabbed);
    assert_eq!(s.recv().await, Some(Msg::Grabbed));
    fake.fail_read();
    assert_eq!(s.recv().await, Some(Msg::Released));
    assert!(!fake.is_grabbed());
}

#[tokio::test]
async fn wrong_psk_and_silent_clients_do_not_block_others() {
    let fake = Fake::default();
    let addr = start(&fake, opts(vec![])).await;
    // A client that connects but never handshakes.
    let _silent = tokio::net::TcpStream::connect(addr).await.unwrap();
    // A client with the wrong key.
    assert!(
        Client::connect_with(addr, Role::Session, &Psk::from_bytes([1u8; 32]))
            .await
            .is_err()
    );
    // A valid handshake followed by something other than Hello.
    let (r, w) = tokio::net::TcpStream::connect(addr)
        .await
        .unwrap()
        .into_split();
    let (r, mut w) = crypto::connect(r, w, &psk()).await.unwrap();
    w.send(&Msg::Ping).await.unwrap();
    let mut bad = Client {
        rx: crypto::spawn_reader(r),
        w,
    };
    assert_eq!(
        bad.recv_raw().await,
        None,
        "server must close a connection without Hello"
    );

    let mut s = Client::session(addr).await;
    assert_eq!(toggle(addr).await, ToggleOutcome::Grabbed);
    assert_eq!(s.recv().await, Some(Msg::Grabbed));
}

/// A session's ping is answered at once when nothing was sent to it for
/// half the heartbeat interval, and not answered right after a send.
#[tokio::test]
async fn answers_session_ping_when_quiet() {
    let fake = Fake::default();
    let mut o = opts(vec![]);
    // Long enough that the periodic ping cannot be mistaken for the answer.
    o.heartbeat_interval = Duration::from_secs(2);
    let addr = start(&fake, o).await;
    let mut s = Client::session(addr).await;
    let registered = Instant::now();

    // The "registered" ping was just sent: no answer.
    s.send(Msg::Ping).await;
    assert!(
        timeout(Duration::from_millis(300), s.rx.recv())
            .await
            .is_err(),
        "ping answered although the server had just sent one"
    );

    // More than half the interval after the last send: answered well before
    // the periodic ping, which is due 2 s after the last send.
    tokio::time::sleep_until(registered + Duration::from_millis(1200)).await;
    s.send(Msg::Ping).await;
    let answer = timeout(Duration::from_millis(500), s.rx.recv())
        .await
        .expect("ping was not answered");
    assert!(matches!(answer, Some(Ok(Msg::Ping))), "{answer:?}");
}

#[tokio::test]
async fn pings_idle_session() {
    let fake = Fake::default();
    let addr = start(&fake, opts(vec![])).await;
    let mut s = Client::session(addr).await;
    assert_eq!(s.recv_raw().await, Some(Msg::Ping));
    assert_eq!(s.recv_raw().await, Some(Msg::Ping));
}

#[tokio::test]
async fn pending_handshakes_are_limited() {
    use tokio::io::AsyncReadExt as _;

    let fake = Fake::default();
    let addr = start(&fake, opts(vec![])).await;
    let extra = 4;
    let mut silent = Vec::new();
    for _ in 0..MAX_PENDING_HANDSHAKES + extra {
        silent.push(tokio::net::TcpStream::connect(addr).await.unwrap());
    }
    // Connections beyond the limit are closed right away; the others stay
    // open until the handshake times out.
    let end = Instant::now() + Duration::from_secs(1);
    let mut closed = 0;
    for s in &mut silent {
        let mut buf = [0u8; 1];
        if let Ok(res) = tokio::time::timeout_at(end, s.read(&mut buf)).await {
            assert!(matches!(res, Ok(0) | Err(_)), "unexpected data: {res:?}");
            closed += 1;
        }
    }
    assert_eq!(closed, extra);

    // Once the silent peers' handshakes time out, a real session gets in.
    let end = Instant::now() + kbd_handoff_core::HANDSHAKE_TIMEOUT + T;
    let mut s = loop {
        if let Ok(mut c) = Client::connect_with(addr, Role::Session, &psk()).await
            && c.recv_raw().await == Some(Msg::Ping)
        {
            break c;
        }
        assert!(Instant::now() < end, "session was never accepted");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(toggle(addr).await, ToggleOutcome::Grabbed);
    assert_eq!(s.recv().await, Some(Msg::Grabbed));
    drop(silent);
}
