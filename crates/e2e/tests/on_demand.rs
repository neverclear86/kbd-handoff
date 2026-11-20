//! End-to-end tests for the on-demand session (`run_on_demand`, used by the
//! receiver's `session` command) against the real sender state machine
//! (`serve`) over localhost TCP with the real Noise channel. Only the
//! keyboard (`KeySource`) and the injection target (`KeySink`) are fakes.

use std::future::pending;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kbd_handoff_core::config::Psk;
use kbd_handoff_core::input::{KeyEvent, KeyValue};
use kbd_handoff_core::proto::ToggleOutcome;
use kbd_handoff_receiver::{ClientOptions, KeySink, NotGrabbed, run_on_demand, toggle};
use kbd_handoff_sender::{GrabbedKeyboard, KeySource, ServerOptions, serve};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};

const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(100);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_millis(400);
/// Upper bound for anything that should happen "promptly".
const T: Duration = Duration::from_secs(5);

// evdev key codes.
const KEY_BACKSPACE: u16 = 14;
const KEY_A: u16 = 30;
const KEY_LEFTCTRL: u16 = 29;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_LEFTALT: u16 = 56;
const CHORD: [u16; 4] = [KEY_LEFTCTRL, KEY_LEFTALT, KEY_LEFTSHIFT, KEY_BACKSPACE];

use KeyValue::{Down, Up};

fn psk() -> Psk {
    Psk::from_bytes([9u8; 32])
}

type KeyTx = mpsc::UnboundedSender<KeyEvent>;

/// Keyboard that can be grabbed (or made to fail) and fed events by hand.
#[derive(Clone, Default)]
struct FakeKeyboard {
    grabbed: Arc<AtomicBool>,
    fail: Arc<AtomicBool>,
    keys: Arc<Mutex<Option<KeyTx>>>,
}

impl FakeKeyboard {
    fn is_grabbed(&self) -> bool {
        self.grabbed.load(Ordering::SeqCst)
    }

    fn press(&self, code: u16, value: KeyValue) {
        assert!(self.is_grabbed(), "press while not grabbed");
        self.keys
            .lock()
            .unwrap()
            .as_ref()
            .expect("no grab channel")
            .send(KeyEvent { code, value })
            .expect("grab dropped");
    }
}

struct FakeGrab {
    rx: mpsc::UnboundedReceiver<KeyEvent>,
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
            Some(ev) => Ok(ev),
            None => pending().await,
        }
    }
}

impl KeySource for FakeKeyboard {
    type Grab = FakeGrab;

    async fn grab(&mut self, _wait: Duration) -> anyhow::Result<FakeGrab> {
        if self.fail.load(Ordering::SeqCst) {
            anyhow::bail!("device not found");
        }
        assert!(
            !self.grabbed.swap(true, Ordering::SeqCst),
            "grabbed twice at the same time"
        );
        let (tx, rx) = mpsc::unbounded_channel();
        *self.keys.lock().unwrap() = Some(tx);
        Ok(FakeGrab {
            rx,
            grabbed: self.grabbed.clone(),
        })
    }
}

#[derive(Clone, Default)]
struct RecordingSink(Arc<Mutex<Vec<(u16, KeyValue)>>>);

impl RecordingSink {
    fn events(&self) -> Vec<(u16, KeyValue)> {
        self.0.lock().unwrap().clone()
    }
}

impl KeySink for RecordingSink {
    fn inject(&mut self, code: u16, value: KeyValue) -> anyhow::Result<()> {
        self.0.lock().unwrap().push((code, value));
        Ok(())
    }
}

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
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let kbd = FakeKeyboard::default();
        let opts = ServerOptions {
            psk: psk(),
            emergency_chord: CHORD.to_vec(),
            heartbeat_interval: HEARTBEAT_INTERVAL,
            heartbeat_timeout: HEARTBEAT_TIMEOUT,
            grab_timeout: Duration::from_secs(1),
        };
        let task = tokio::spawn(serve(listener, kbd.clone(), opts));
        Self { addr, kbd, task }
    }

    fn client(&self) -> ClientOptions {
        ClientOptions {
            server: self.addr.to_string(),
            psk: psk(),
            heartbeat_interval: HEARTBEAT_INTERVAL,
            heartbeat_timeout: HEARTBEAT_TIMEOUT,
            reconnect_min: Duration::from_millis(100),
            reconnect_max: Duration::from_millis(500),
        }
    }

    fn spawn_on_demand(&self, sink: &RecordingSink) -> JoinHandle<anyhow::Result<()>> {
        let opts = self.client();
        let mut sink = sink.clone();
        tokio::spawn(async move { run_on_demand(&opts, &mut sink).await })
    }

    async fn toggle(&self) -> ToggleOutcome {
        timeout(T, toggle(&self.client()))
            .await
            .expect("toggle hung")
            .expect("toggle failed")
    }
}

async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + T;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        sleep(Duration::from_millis(5)).await;
    }
}

async fn join(h: JoinHandle<anyhow::Result<()>>) -> anyhow::Result<()> {
    timeout(T, h)
        .await
        .expect("session did not end")
        .expect("session task panicked")
}

/// No resident session: `toggle` reports `NoSession`; the on-demand session
/// then grabs by itself, forwards keys, and ends after the next toggle.
#[tokio::test]
async fn on_demand_session_grabs_forwards_and_ends_on_toggle() {
    let s = Sender::start().await;
    assert_eq!(s.toggle().await, ToggleOutcome::NoSession);
    assert!(!s.kbd.is_grabbed());

    let sink = RecordingSink::default();
    let session = s.spawn_on_demand(&sink);
    wait_until("grabbed by the session's own request", || {
        s.kbd.is_grabbed()
    })
    .await;

    s.kbd.press(KEY_LEFTSHIFT, Down);
    s.kbd.press(KEY_A, Down);
    s.kbd.press(KEY_A, Up);
    wait_until("keys forwarded", || sink.events().len() == 3).await;

    // The second toggle hands the keyboard back while Shift is held.
    assert_eq!(s.toggle().await, ToggleOutcome::Released);
    join(session)
        .await
        .expect("session must end cleanly on Released");
    assert!(!s.kbd.is_grabbed());
    assert_eq!(
        sink.events(),
        [
            (KEY_LEFTSHIFT, Down),
            (KEY_A, Down),
            (KEY_A, Up),
            (KEY_LEFTSHIFT, Up),
        ]
    );

    // The session is gone, so the next toggle starts over.
    wait_until_no_session(&s).await;
}

/// `toggle` may race with the sender noticing the closed session; retry
/// until it reports `NoSession` (which never grabs).
async fn wait_until_no_session(s: &Sender) {
    let deadline = Instant::now() + T;
    loop {
        match s.toggle().await {
            ToggleOutcome::NoSession => return,
            other => {
                assert_ne!(other, ToggleOutcome::Grabbed, "grabbed without a session");
                assert!(Instant::now() < deadline, "session still registered");
                sleep(Duration::from_millis(20)).await;
            }
        }
    }
}

/// The emergency chord ends the on-demand session too.
#[tokio::test]
async fn on_demand_session_ends_on_emergency_chord() {
    let s = Sender::start().await;
    let sink = RecordingSink::default();
    let session = s.spawn_on_demand(&sink);
    wait_until("grabbed", || s.kbd.is_grabbed()).await;
    for code in CHORD {
        s.kbd.press(code, Down);
    }
    join(session).await.expect("session must end on Released");
    assert!(!s.kbd.is_grabbed());
    // The chord's last key is not forwarded; the others are released in
    // ascending key code order.
    assert_eq!(
        sink.events(),
        [
            (KEY_LEFTCTRL, Down),
            (KEY_LEFTALT, Down),
            (KEY_LEFTSHIFT, Down),
            (KEY_LEFTCTRL, Up),
            (KEY_LEFTSHIFT, Up),
            (KEY_LEFTALT, Up),
        ]
    );
}

/// A failed grab ends the session with `NotGrabbed(Failed)`.
#[tokio::test]
async fn on_demand_session_reports_failed_grab() {
    let s = Sender::start().await;
    s.kbd.fail.store(true, Ordering::SeqCst);
    let sink = RecordingSink::default();
    let err = join(s.spawn_on_demand(&sink)).await.unwrap_err();
    assert_eq!(
        err.downcast_ref::<NotGrabbed>(),
        Some(&NotGrabbed(ToggleOutcome::Failed)),
        "{err:#}"
    );
    assert!(!s.kbd.is_grabbed());
    assert!(sink.events().is_empty());
}
