//! End-to-end tests: the real sender state machine (`serve`) and the real
//! receiver (`run_session`, `toggle`) talking over localhost TCP with the real
//! Noise channel. Only the keyboard (`KeySource`) and the injection target
//! (`KeySink`) are fakes.

use std::collections::HashSet;
use std::future::{Future, pending};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kbd_handoff_core::config::Psk;
use kbd_handoff_core::crypto::{self, ReaderChannel, SecureWriter};
use kbd_handoff_core::input::{KeyEvent, KeyValue};
use kbd_handoff_core::proto::{Msg, Role, ToggleOutcome};
use kbd_handoff_receiver::{ClientOptions, KeySink, run_session, toggle};
use kbd_handoff_sender::{GrabbedKeyboard, KeySource, ServerOptions, serve};
use tokio::net::TcpListener;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};

const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(100);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_millis(400);
/// Upper bound for anything that should happen "promptly".
const T: Duration = Duration::from_secs(5);
/// Allowed scheduling slack on top of a protocol deadline.
const SLACK: Duration = Duration::from_millis(300);

// evdev key codes.
const KEY_BACKSPACE: u16 = 14;
const KEY_A: u16 = 30;
const KEY_S: u16 = 31;
const KEY_D: u16 = 32;
const KEY_LEFTCTRL: u16 = 29;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_LEFTALT: u16 = 56;
/// Same as the sender's default chord.
const CHORD: [u16; 4] = [KEY_LEFTCTRL, KEY_LEFTALT, KEY_LEFTSHIFT, KEY_BACKSPACE];

use KeyValue::{Down, Repeat, Up};

fn psk() -> Psk {
    Psk::from_bytes([7u8; 32])
}

fn wrong_psk() -> Psk {
    Psk::from_bytes([8u8; 32])
}

// ---------------------------------------------------------------------------
// Fake keyboard
// ---------------------------------------------------------------------------

type KeyTx = mpsc::UnboundedSender<anyhow::Result<KeyEvent>>;

/// Scripted keyboard. Events are pushed through [`FakeKeyboard::press`] while
/// grabbed. Keys in `held` are "physically held"; `grab` waits until the set
/// is empty, like the real source, and gives up after its timeout.
#[derive(Clone, Default)]
struct FakeKeyboard {
    opens: Arc<AtomicUsize>,
    grabbed: Arc<AtomicBool>,
    held: Arc<Mutex<HashSet<u16>>>,
    keys: Arc<Mutex<Option<KeyTx>>>,
}

impl FakeKeyboard {
    fn opens(&self) -> usize {
        self.opens.load(Ordering::SeqCst)
    }

    fn is_grabbed(&self) -> bool {
        self.grabbed.load(Ordering::SeqCst)
    }

    fn hold(&self, code: u16) {
        self.held.lock().unwrap().insert(code);
    }

    fn release_held(&self) {
        self.held.lock().unwrap().clear();
    }

    fn press(&self, code: u16, value: KeyValue) {
        assert!(
            self.is_grabbed(),
            "press({code}, {value:?}) while not grabbed"
        );
        self.keys
            .lock()
            .unwrap()
            .as_ref()
            .expect("no grab channel")
            .send(Ok(KeyEvent { code, value }))
            .expect("grab dropped");
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

impl GrabbedKeyboard for FakeGrab {
    async fn next_event(&mut self) -> anyhow::Result<KeyEvent> {
        match self.rx.recv().await {
            Some(r) => r,
            None => pending().await,
        }
    }
}

impl KeySource for FakeKeyboard {
    type Grab = FakeGrab;

    fn grab(&mut self, wait: Duration) -> impl Future<Output = anyhow::Result<FakeGrab>> + Send {
        let this = self.clone();
        async move {
            this.opens.fetch_add(1, Ordering::SeqCst);
            let deadline = Instant::now() + wait;
            while !this.held.lock().unwrap().is_empty() {
                if Instant::now() >= deadline {
                    anyhow::bail!("keys still held after {wait:?}");
                }
                sleep(Duration::from_millis(10)).await;
            }
            assert!(
                !this.grabbed.swap(true, Ordering::SeqCst),
                "grabbed twice at the same time"
            );
            let (tx, rx) = mpsc::unbounded_channel();
            *this.keys.lock().unwrap() = Some(tx);
            Ok(FakeGrab {
                rx,
                grabbed: this.grabbed.clone(),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Recording sink
// ---------------------------------------------------------------------------

/// Records every injected event. Clones share the log, so it stays readable
/// after the session task that owns one clone is aborted.
#[derive(Clone, Default)]
struct RecordingSink {
    log: Arc<Mutex<Vec<(u16, KeyValue)>>>,
}

impl RecordingSink {
    fn events(&self) -> Vec<(u16, KeyValue)> {
        self.log.lock().unwrap().clone()
    }

    fn len(&self) -> usize {
        self.log.lock().unwrap().len()
    }
}

impl KeySink for RecordingSink {
    fn inject(&mut self, code: u16, value: KeyValue) -> anyhow::Result<()> {
        self.log.lock().unwrap().push((code, value));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn server_opts() -> ServerOptions {
    ServerOptions {
        psk: psk(),
        emergency_chord: CHORD.to_vec(),
        heartbeat_interval: HEARTBEAT_INTERVAL,
        heartbeat_timeout: HEARTBEAT_TIMEOUT,
        grab_timeout: Duration::from_secs(1),
    }
}

fn client_opts(addr: SocketAddr, psk: Psk) -> ClientOptions {
    ClientOptions {
        server: addr.to_string(),
        psk,
        heartbeat_interval: HEARTBEAT_INTERVAL,
        heartbeat_timeout: HEARTBEAT_TIMEOUT,
        reconnect_min: Duration::from_millis(100),
        reconnect_max: Duration::from_millis(500),
    }
}

/// A running sender. Aborts `serve` (which drops any grab) when dropped.
struct Sender {
    addr: SocketAddr,
    kbd: FakeKeyboard,
    task: JoinHandle<anyhow::Result<()>>,
}

impl Drop for Sender {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Sender {
    async fn start(opts: ServerOptions) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let kbd = FakeKeyboard::default();
        let task = tokio::spawn(serve(listener, kbd.clone(), opts));
        Self { addr, kbd, task }
    }

    fn client(&self) -> ClientOptions {
        client_opts(self.addr, psk())
    }

    /// Start a receiver session in a task.
    fn spawn_session(&self, sink: &RecordingSink) -> JoinHandle<anyhow::Result<()>> {
        let opts = self.client();
        let mut sink = sink.clone();
        tokio::spawn(async move { run_session(&opts, &mut sink).await })
    }

    async fn toggle(&self) -> ToggleOutcome {
        timeout(T, toggle(&self.client()))
            .await
            .expect("toggle hung")
            .expect("toggle failed")
    }

    /// Toggle until a session is registered (`NoSession` never opens the
    /// device, so retrying is harmless). Returns the first other outcome.
    async fn toggle_when_registered(&self) -> ToggleOutcome {
        let deadline = Instant::now() + T;
        loop {
            match self.toggle().await {
                ToggleOutcome::NoSession => {
                    assert!(Instant::now() < deadline, "session never registered");
                    sleep(Duration::from_millis(20)).await;
                }
                other => return other,
            }
        }
    }
}

/// Poll `cond` until it holds, panicking after `limit`.
async fn wait_until(what: &str, limit: Duration, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        sleep(Duration::from_millis(5)).await;
    }
}

/// Poll until the grab flag is cleared and return when that was observed.
async fn wait_ungrabbed(kbd: &FakeKeyboard, limit: Duration) -> Instant {
    wait_until("grab dropped", limit, || !kbd.is_grabbed()).await;
    Instant::now()
}

/// A hand-driven client speaking the protocol directly, used where the test
/// needs to see raw messages or to stop sending pings.
struct RawClient {
    rx: ReaderChannel,
    w: SecureWriter<OwnedWriteHalf>,
}

impl RawClient {
    async fn connect(addr: SocketAddr, role: Role) -> Self {
        let (r, w) = tokio::net::TcpStream::connect(addr)
            .await
            .unwrap()
            .into_split();
        let (r, mut w) = crypto::connect(r, w, &psk()).await.unwrap();
        w.send(&Msg::Hello { role }).await.unwrap();
        Self {
            rx: crypto::spawn_reader(r),
            w,
        }
    }

    /// Connect as a session and wait for the "registered" ping.
    async fn session(addr: SocketAddr) -> Self {
        let mut c = Self::connect(addr, Role::Session).await;
        assert_eq!(
            c.recv().await,
            Msg::Ping,
            "first message must be the registration ping"
        );
        c
    }

    async fn recv(&mut self) -> Msg {
        timeout(T, self.rx.recv())
            .await
            .expect("no message from the sender")
            .expect("connection closed")
            .expect("connection error")
    }

    /// Next message that is not a `Ping`.
    async fn recv_non_ping(&mut self) -> Msg {
        loop {
            match self.recv().await {
                Msg::Ping => continue,
                m => return m,
            }
        }
    }

    async fn ping(&mut self) {
        self.w.send(&Msg::Ping).await.unwrap();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// 1. toggle on -> keys forwarded -> toggle off -> every held key is released
///    on the receiver and the grab is dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn toggle_forwards_keys_and_releases_held_keys_on_toggle_off() {
    let s = Sender::start(server_opts()).await;
    let sink = RecordingSink::default();
    let session = s.spawn_session(&sink);

    assert_eq!(s.toggle_when_registered().await, ToggleOutcome::Grabbed);
    assert!(s.kbd.is_grabbed());
    assert_eq!(s.kbd.opens(), 1);

    s.kbd.press(KEY_A, Down);
    s.kbd.press(KEY_S, Down);
    s.kbd.press(KEY_A, Up);
    s.kbd.press(KEY_S, Repeat);
    s.kbd.press(KEY_D, Down);
    wait_until("5 key events forwarded", T, || sink.len() == 5).await;
    assert_eq!(
        sink.events(),
        vec![
            (KEY_A, Down),
            (KEY_S, Down),
            (KEY_A, Up),
            (KEY_S, Repeat),
            (KEY_D, Down)
        ]
    );

    assert_eq!(s.toggle().await, ToggleOutcome::Released);
    assert!(
        !s.kbd.is_grabbed(),
        "grab must be dropped once Released is reported"
    );

    // S and D were still held: both are released (in key code order).
    wait_until("held keys released", T, || sink.len() == 7).await;
    assert_eq!(sink.events()[5..], [(KEY_S, Up), (KEY_D, Up)]);

    // The session survives and a second round works.
    assert!(!session.is_finished());
    assert_eq!(s.toggle().await, ToggleOutcome::Grabbed);
    assert_eq!(s.kbd.opens(), 2);
    assert_eq!(s.toggle().await, ToggleOutcome::Released);
    assert!(!s.kbd.is_grabbed());
    session.abort();
}

/// 2. A wrong PSK is rejected both for sessions and for toggle requests, and
///    nothing is grabbed even while a legitimate session is connected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_psk_is_rejected() {
    let s = Sender::start(server_opts()).await;
    let bad = client_opts(s.addr, wrong_psk());

    // Session with the wrong key: handshake fails, run_session returns Err.
    let mut bad_sink = RecordingSink::default();
    let res = timeout(T, run_session(&bad, &mut bad_sink))
        .await
        .expect("bad session hung");
    let err = res.expect_err("session with a wrong PSK must fail");
    assert!(
        format!("{err:#}").contains("handshake"),
        "unexpected error: {err:#}"
    );

    // With no good session, a wrong-key toggle does not even reach the state machine.
    assert!(
        timeout(T, toggle(&bad))
            .await
            .expect("toggle hung")
            .is_err()
    );

    // Connect a legitimate session, prove it is registered without grabbing:
    // grab and release once, then reset the counter baseline.
    let sink = RecordingSink::default();
    let session = s.spawn_session(&sink);
    assert_eq!(s.toggle_when_registered().await, ToggleOutcome::Grabbed);
    assert_eq!(s.toggle().await, ToggleOutcome::Released);
    let opens = s.kbd.opens();

    // Wrong-key toggle while a session is live: rejected, nothing opened.
    for _ in 0..3 {
        let res = timeout(T, toggle(&bad)).await.expect("toggle hung");
        assert!(
            res.is_err(),
            "toggle with a wrong PSK must fail, got {res:?}"
        );
    }
    assert_eq!(s.kbd.opens(), opens);
    assert!(!s.kbd.is_grabbed());

    // A wrong-key session must not have replaced the good one.
    let mut bad_sink = RecordingSink::default();
    assert!(
        timeout(T, run_session(&bad, &mut bad_sink))
            .await
            .unwrap()
            .is_err()
    );
    assert!(!session.is_finished(), "good session must survive");
    assert_eq!(s.toggle().await, ToggleOutcome::Grabbed);
    assert_eq!(s.toggle().await, ToggleOutcome::Released);
    assert!(bad_sink.events().is_empty());
    session.abort();
}

/// 3. A toggle with no session connected returns NoSession and never opens
///    the device.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn toggle_without_session_reports_no_session() {
    let s = Sender::start(server_opts()).await;
    for _ in 0..3 {
        assert_eq!(s.toggle().await, ToggleOutcome::NoSession);
    }
    assert_eq!(s.kbd.opens(), 0);
    assert!(!s.kbd.is_grabbed());
}

/// 4. Killing the receiver while forwarding returns the keyboard to Linux
///    within the heartbeat timeout, and the receiver releases its held keys.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receiver_abort_while_forwarding_drops_grab() {
    let s = Sender::start(server_opts()).await;
    let sink = RecordingSink::default();
    let session = s.spawn_session(&sink);
    assert_eq!(s.toggle_when_registered().await, ToggleOutcome::Grabbed);

    s.kbd.press(KEY_A, Down);
    wait_until("key forwarded", T, || sink.len() == 1).await;

    session.abort();
    let aborted = Instant::now();
    let released = wait_ungrabbed(&s.kbd, T).await;
    let took = released - aborted;
    assert!(
        took <= HEARTBEAT_TIMEOUT + SLACK,
        "grab dropped after {took:?}, expected within {HEARTBEAT_TIMEOUT:?}"
    );

    // Dropping the session future released the key that was held on Windows.
    assert_eq!(sink.events(), vec![(KEY_A, Down), (KEY_A, Up)]);

    // With the session gone, toggle no longer grabs.
    assert_eq!(s.toggle().await, ToggleOutcome::NoSession);
    assert_eq!(s.kbd.opens(), 1);
}

/// 5. A session that stops sending (but keeps the TCP connection open) is
///    dropped after the heartbeat timeout, and the grab with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heartbeat_loss_drops_grab() {
    let s = Sender::start(server_opts()).await;
    let mut raw = RawClient::session(s.addr).await;

    // Keep pinging while the toggle is in flight, then go silent.
    let toggle_fut = s.toggle();
    tokio::pin!(toggle_fut);
    let mut iv = tokio::time::interval(HEARTBEAT_INTERVAL);
    let outcome = loop {
        tokio::select! {
            o = &mut toggle_fut => break o,
            _ = iv.tick() => raw.ping().await,
        }
    };
    assert_eq!(outcome, ToggleOutcome::Grabbed);
    // Taken before sending, so the measured silence is a lower bound.
    let last_ping = Instant::now();
    raw.ping().await;
    assert!(s.kbd.is_grabbed());

    let released = wait_ungrabbed(&s.kbd, T).await;
    let silent_for = released - last_ping;
    assert!(
        silent_for >= HEARTBEAT_TIMEOUT,
        "grab dropped after only {silent_for:?} of silence"
    );
    assert!(
        silent_for <= HEARTBEAT_TIMEOUT + SLACK,
        "grab dropped after {silent_for:?} of silence, expected about {HEARTBEAT_TIMEOUT:?}"
    );

    // The silent session is gone.
    assert_eq!(s.toggle().await, ToggleOutcome::NoSession);
    // Both halves were kept alive until here, so this was a timeout, not a close.
    drop(raw);
}

/// 6a. The emergency chord releases the grab; the receiver gets Released and
///     lets go of every forwarded key. The chord's last key is not forwarded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emergency_chord_releases_grab() {
    let s = Sender::start(server_opts()).await;
    let sink = RecordingSink::default();
    let session = s.spawn_session(&sink);
    assert_eq!(s.toggle_when_registered().await, ToggleOutcome::Grabbed);

    s.kbd.press(KEY_A, Down);
    s.kbd.press(KEY_LEFTCTRL, Down);
    s.kbd.press(KEY_LEFTALT, Down);
    s.kbd.press(KEY_LEFTSHIFT, Down);
    s.kbd.press(KEY_BACKSPACE, Down);
    wait_ungrabbed(&s.kbd, T).await;

    // 4 forwarded downs, then Up for each of them in key code order.
    wait_until("held keys released", T, || sink.len() == 8).await;
    assert_eq!(
        sink.events(),
        vec![
            (KEY_A, Down),
            (KEY_LEFTCTRL, Down),
            (KEY_LEFTALT, Down),
            (KEY_LEFTSHIFT, Down),
            (KEY_LEFTCTRL, Up),
            (KEY_A, Up),
            (KEY_LEFTSHIFT, Up),
            (KEY_LEFTALT, Up),
        ]
    );
    assert!(!sink.events().contains(&(KEY_BACKSPACE, Down)));

    // The connection stayed up, so the Ups came from Released, not from a
    // disconnect; a new toggle grabs again.
    sleep(HEARTBEAT_TIMEOUT + SLACK).await;
    assert!(!session.is_finished(), "session must survive the chord");
    assert_eq!(s.toggle().await, ToggleOutcome::Grabbed);
    assert_eq!(s.kbd.opens(), 2);
    assert_eq!(s.toggle().await, ToggleOutcome::Released);
    session.abort();
}

/// 6b. The chord works even when the receiver is unresponsive: a session that
///     never pings, with a heartbeat timeout too long to explain the release.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emergency_chord_works_with_silent_receiver() {
    let opts = ServerOptions {
        heartbeat_timeout: Duration::from_secs(30),
        ..server_opts()
    };
    let s = Sender::start(opts).await;
    let mut raw = RawClient::session(s.addr).await;
    assert_eq!(s.toggle().await, ToggleOutcome::Grabbed);

    for code in CHORD {
        s.kbd.press(code, Down);
    }
    let pressed = Instant::now();
    let released = wait_ungrabbed(&s.kbd, T).await;
    assert!(
        released - pressed <= SLACK,
        "chord took {:?}",
        released - pressed
    );

    // On the wire: Grabbed, three chord keys, Released. No Backspace.
    assert_eq!(raw.recv_non_ping().await, Msg::Grabbed);
    for code in &CHORD[..3] {
        assert_eq!(
            raw.recv_non_ping().await,
            Msg::Key(KeyEvent {
                code: *code,
                value: Down
            })
        );
    }
    assert_eq!(raw.recv_non_ping().await, Msg::Released);
}

/// 7. Keys held when the grab is requested: the grab waits for them, and gives
///    up with Failed (not left grabbed) after grab_timeout. A grab_timeout
///    longer than the heartbeat timeout must not kill the session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keys_held_at_grab_time() {
    let grab_timeout = Duration::from_millis(600);
    assert!(grab_timeout > HEARTBEAT_TIMEOUT);
    let s = Sender::start(ServerOptions {
        grab_timeout,
        ..server_opts()
    })
    .await;
    let sink = RecordingSink::default();
    let session = s.spawn_session(&sink);
    // Register the session with nothing held, then return the keyboard.
    assert_eq!(s.toggle_when_registered().await, ToggleOutcome::Grabbed);
    assert_eq!(s.toggle().await, ToggleOutcome::Released);

    // Held for longer than grab_timeout: Failed.
    s.kbd.hold(KEY_A);
    let started = Instant::now();
    assert_eq!(s.toggle().await, ToggleOutcome::Failed);
    assert!(started.elapsed() >= grab_timeout);
    assert!(!s.kbd.is_grabbed());
    assert_eq!(s.kbd.opens(), 2);

    // Released partway through the wait: Grabbed once the key is up.
    let kbd = s.kbd.clone();
    let started = Instant::now();
    let releaser = tokio::spawn(async move {
        sleep(Duration::from_millis(200)).await;
        kbd.release_held();
    });
    assert_eq!(s.toggle().await, ToggleOutcome::Grabbed);
    assert!(started.elapsed() >= Duration::from_millis(200));
    releaser.await.unwrap();

    // The session survived the blocked waits and still receives keys.
    assert!(!session.is_finished());
    s.kbd.press(KEY_S, Down);
    wait_until("key forwarded", T, || {
        sink.events().last() == Some(&(KEY_S, Down))
    })
    .await;
    assert_eq!(s.toggle().await, ToggleOutcome::Released);
    assert!(!s.kbd.is_grabbed());
    session.abort();
}

/// 8. An idle sender never opens the input device, even with a session
///    connected and heartbeats flowing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_sender_never_opens_device() {
    let s = Sender::start(server_opts()).await;
    let sink = RecordingSink::default();
    let session = s.spawn_session(&sink);

    // Many heartbeat intervals and more than two heartbeat timeouts.
    sleep(HEARTBEAT_TIMEOUT * 3).await;
    assert_eq!(s.kbd.opens(), 0);
    assert!(!s.kbd.is_grabbed());
    assert!(
        !session.is_finished(),
        "session must stay connected while idle"
    );
    assert!(sink.events().is_empty());

    // The session was live the whole time: the first toggle grabs.
    assert_eq!(s.toggle().await, ToggleOutcome::Grabbed);
    assert_eq!(s.kbd.opens(), 1);
    assert_eq!(s.toggle().await, ToggleOutcome::Released);
    session.abort();
}

/// 9. A new session replaces the old one while forwarding: the old one gets
///    Released, the grab is dropped, and the new session works.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_session_replaces_first_while_forwarding() {
    // The raw first session never pings; a long timeout keeps it registered.
    let s = Sender::start(ServerOptions {
        heartbeat_timeout: Duration::from_secs(30),
        ..server_opts()
    })
    .await;
    let mut first = RawClient::session(s.addr).await;
    assert_eq!(s.toggle().await, ToggleOutcome::Grabbed);
    s.kbd.press(KEY_A, Down);
    assert_eq!(first.recv_non_ping().await, Msg::Grabbed);
    assert_eq!(
        first.recv_non_ping().await,
        Msg::Key(KeyEvent {
            code: KEY_A,
            value: Down
        })
    );

    let sink = RecordingSink::default();
    let second = s.spawn_session(&sink);
    wait_ungrabbed(&s.kbd, T).await;

    // The first session is told to release, then disconnected.
    assert_eq!(first.recv_non_ping().await, Msg::Released);
    let end = timeout(T, async {
        loop {
            match first.rx.recv().await {
                Some(Ok(Msg::Ping)) => continue,
                other => return other,
            }
        }
    })
    .await
    .expect("first session not disconnected");
    assert!(
        matches!(end, None | Some(Err(_))),
        "expected disconnect, got {end:?}"
    );

    // Keys now go to the second session.
    assert_eq!(s.toggle_when_registered().await, ToggleOutcome::Grabbed);
    s.kbd.press(KEY_S, Down);
    wait_until("key forwarded to second", T, || sink.len() == 1).await;
    assert_eq!(s.toggle().await, ToggleOutcome::Released);
    wait_until("held key released", T, || sink.len() == 2).await;
    assert_eq!(sink.events(), vec![(KEY_S, Down), (KEY_S, Up)]);
    second.abort();
}

/// 9b. Same with a real receiver as the first session: its held keys are
///     released and its run_session ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_session_replaces_first_real_receiver() {
    let s = Sender::start(server_opts()).await;
    let sink1 = RecordingSink::default();
    let first = s.spawn_session(&sink1);
    assert_eq!(s.toggle_when_registered().await, ToggleOutcome::Grabbed);
    s.kbd.press(KEY_A, Down);
    wait_until("key forwarded", T, || sink1.len() == 1).await;

    let sink2 = RecordingSink::default();
    let second = s.spawn_session(&sink2);
    wait_ungrabbed(&s.kbd, T).await;
    wait_until("first session released keys", T, || sink1.len() == 2).await;
    assert_eq!(sink1.events(), vec![(KEY_A, Down), (KEY_A, Up)]);
    let res = timeout(T, first)
        .await
        .expect("first session still running")
        .unwrap();
    assert!(res.is_err());
    assert!(sink2.events().is_empty());
    assert!(!second.is_finished());
    second.abort();
}
