//! Linux sender: waits for requests from the Windows receiver, grabs the
//! keyboard on demand and forwards key events.
//!
//! The state machine in [`serve`] is written against the [`KeySource`] and
//! [`GrabbedKeyboard`] traits. [`EvdevSource`] (Linux only) is the real
//! implementation; tests use an in-memory fake.
//!
//! # Connection flow
//!
//! Every client completes a Noise handshake and then sends
//! `Msg::Hello { role }`.
//!
//! * `Role::Session`: the resident receiver. Only one session exists at a
//!   time; a new one replaces the old. As soon as a session is registered the
//!   server sends it a `Msg::Ping`, so the first `Ping` a session receives
//!   means "registered" (toggle requests sent before that may see
//!   `NoSession`). Afterwards the server pings whenever it has had nothing
//!   to send for `heartbeat_interval`. It also answers a `Ping` from the
//!   session with a `Ping` of its own if it has sent nothing for half that
//!   interval. With a receiver that pings at the same interval, the server's
//!   pings then go out on the wakeups caused by the receiver's pings, and
//!   the server's own timer rarely has to fire.
//! * `Role::Toggle`: sends one `ToggleRequest`, receives one `ToggleResult`
//!   and the connection is closed.

use std::collections::HashSet;
use std::future::{Future, pending};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use kbd_handoff_core::config::Psk;
use kbd_handoff_core::crypto::{self, SecureWriter};
use kbd_handoff_core::input::{KeyEvent, KeyValue};
use kbd_handoff_core::proto::{Msg, Role, ToggleOutcome};
use tokio::net::TcpListener;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio::time::{Instant, sleep_until, timeout};
use tracing::{debug, info, warn};

#[cfg(target_os = "linux")]
mod evdev_source;
#[cfg(target_os = "linux")]
pub use evdev_source::{EvdevGrab, EvdevSource, list_keyboards, parse_key_name};

/// Default emergency release chord, as evdev key names.
pub const DEFAULT_EMERGENCY_CHORD: [&str; 4] = [
    "KEY_LEFTCTRL",
    "KEY_LEFTALT",
    "KEY_LEFTSHIFT",
    "KEY_BACKSPACE",
];

/// Default time to wait for all keys to be released before grabbing.
pub const DEFAULT_GRAB_TIMEOUT: Duration = Duration::from_secs(3);

/// Capacity of the per-session outgoing queue. If the receiver stops reading
/// and this fills up, the session is considered dead.
const SESSION_QUEUE: usize = 1024;

/// Maximum number of connections that may be in the handshake / `Hello`
/// phase at once. Further connections are closed immediately, which bounds
/// what unauthenticated peers can make the sender hold.
const MAX_PENDING_HANDSHAKES: usize = 16;

/// How long to stop accepting after `accept` fails (e.g. out of file
/// descriptors), so the loop does not spin.
const ACCEPT_ERROR_PAUSE: Duration = Duration::from_millis(100);

/// A keyboard that can be grabbed on demand.
pub trait KeySource: Send + 'static {
    type Grab: GrabbedKeyboard;
    /// Open the device, wait until no key is held (give up after `timeout`), then grab it exclusively.
    fn grab(
        &mut self,
        timeout: Duration,
    ) -> impl Future<Output = anyhow::Result<Self::Grab>> + Send;
}

/// An exclusively grabbed keyboard. Dropping it releases the grab and closes
/// the device.
pub trait GrabbedKeyboard: Send + 'static {
    /// Next EV_KEY event. MUST be cancel-safe (used in `tokio::select!`).
    fn next_event(&mut self) -> impl Future<Output = anyhow::Result<KeyEvent>> + Send;
}

/// Parameters for [`serve`].
#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub psk: Psk,
    /// Evdev key codes that, held down together, release the grab without
    /// involving the receiver. An empty list disables the chord.
    pub emergency_chord: Vec<u16>,
    /// The server pings a session after this long without sending anything.
    pub heartbeat_interval: Duration,
    /// A session that sends no frame for this long is dropped.
    pub heartbeat_timeout: Duration,
    /// Passed to [`KeySource::grab`].
    pub grab_timeout: Duration,
}

/// Messages from connection tasks to the main loop.
enum Event {
    Session {
        peer: SocketAddr,
        writer: SecureWriter<OwnedWriteHalf>,
        rx: crypto::ReaderChannel,
    },
    Toggle {
        peer: SocketAddr,
        reply: oneshot::Sender<ToggleOutcome>,
    },
}

struct Session {
    id: u64,
    peer: SocketAddr,
    tx: mpsc::Sender<Msg>,
    rx: crypto::ReaderChannel,
    last_rx: Instant,
    /// When a message was last queued for the session.
    last_tx: Instant,
}

enum SessionEvent {
    Msg(Msg),
    Error(crypto::Error),
    Closed,
    Timeout,
    WriterGone,
}

/// Run the sender until the listener fails irrecoverably (in practice: forever).
///
/// Dropping the returned future releases any grab it holds.
pub async fn serve<S: KeySource>(
    listener: TcpListener,
    source: S,
    opts: ServerOptions,
) -> anyhow::Result<()> {
    let (events_tx, mut events_rx) = mpsc::channel::<Event>(64);
    let conn_opts = Arc::new(ConnOptions {
        psk: opts.psk.clone(),
        handshake_wait: opts.heartbeat_timeout,
    });
    let handshakes = Arc::new(Semaphore::new(MAX_PENDING_HANDSHAKES));
    // Set after an accept error; the accept arm waits until then without
    // delaying the other arms.
    let mut accept_resume: Option<Instant> = None;
    let mut server = Server {
        source,
        opts,
        session: None,
        grab: None,
        pressed: HashSet::new(),
        next_session_id: 0,
    };
    if let Ok(addr) = listener.local_addr() {
        info!(%addr, "listening");
    }

    loop {
        let deadline = server
            .session
            .as_ref()
            .map(|s| s.last_rx + server.opts.heartbeat_timeout);
        tokio::select! {
            biased;
            ev = next_key(&mut server.grab) => server.on_key(ev),
            ev = session_event(&mut server.session, deadline) => server.on_session_event(ev).await,
            ev = events_rx.recv() => {
                // `events_tx` is held by this function, so the channel never closes.
                if let Some(ev) = ev {
                    server.on_event(ev).await;
                }
            }
            res = accept_after(&listener, accept_resume) => match res {
                Ok((stream, peer)) => {
                    accept_resume = None;
                    let Ok(permit) = handshakes.clone().try_acquire_owned() else {
                        debug!(%peer, "too many pending handshakes; connection dropped");
                        continue;
                    };
                    let events = events_tx.clone();
                    let conn_opts = conn_opts.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            handle_connection(stream, peer, &conn_opts, events, permit).await
                        {
                            warn!(%peer, "connection rejected: {e:#}");
                        }
                    });
                }
                Err(e) => {
                    warn!("accept failed: {e}");
                    accept_resume = Some(Instant::now() + ACCEPT_ERROR_PAUSE);
                }
            },
        }
    }
}

/// Accept a connection, first waiting until `resume` if it is set.
/// Cancel-safe.
async fn accept_after(
    listener: &TcpListener,
    resume: Option<Instant>,
) -> std::io::Result<(tokio::net::TcpStream, SocketAddr)> {
    if let Some(t) = resume {
        sleep_until(t).await;
    }
    listener.accept().await
}

async fn next_key<G: GrabbedKeyboard>(grab: &mut Option<G>) -> anyhow::Result<KeyEvent> {
    match grab {
        Some(g) => g.next_event().await,
        None => pending().await,
    }
}

async fn session_event(session: &mut Option<Session>, deadline: Option<Instant>) -> SessionEvent {
    let (Some(s), Some(deadline)) = (session, deadline) else {
        return pending().await;
    };
    let Session { tx, rx, .. } = s;
    tokio::select! {
        biased;
        m = rx.recv() => match m {
            Some(Ok(m)) => SessionEvent::Msg(m),
            Some(Err(e)) => SessionEvent::Error(e),
            None => SessionEvent::Closed,
        },
        _ = tx.closed() => SessionEvent::WriterGone,
        _ = sleep_until(deadline) => SessionEvent::Timeout,
    }
}

struct Server<S: KeySource> {
    source: S,
    opts: ServerOptions,
    session: Option<Session>,
    grab: Option<S::Grab>,
    pressed: HashSet<u16>,
    next_session_id: u64,
}

impl<S: KeySource> Server<S> {
    fn on_key(&mut self, ev: anyhow::Result<KeyEvent>) {
        let ev = match ev {
            Ok(ev) => ev,
            Err(e) => {
                warn!("keyboard read failed: {e:#}");
                self.release("keyboard read failed");
                return;
            }
        };
        match ev.value {
            KeyValue::Down | KeyValue::Repeat => {
                self.pressed.insert(ev.code);
            }
            KeyValue::Up => {
                self.pressed.remove(&ev.code);
            }
        }
        let chord = &self.opts.emergency_chord;
        if ev.value == KeyValue::Down
            && !chord.is_empty()
            && chord.contains(&ev.code)
            && chord.iter().all(|c| self.pressed.contains(c))
        {
            warn!("emergency release chord pressed");
            self.release("emergency chord");
            return;
        }
        self.send_to_session(Msg::Key(ev));
    }

    async fn on_session_event(&mut self, ev: SessionEvent) {
        let reason = match ev {
            SessionEvent::Msg(m) => {
                if let Some(s) = &mut self.session {
                    s.last_rx = Instant::now();
                }
                match m {
                    Msg::Ping => self.answer_ping(),
                    Msg::ToggleRequest => {
                        let outcome = self.toggle().await;
                        self.send_to_session(Msg::ToggleResult(outcome));
                    }
                    other => warn!("unexpected message from session ignored: {other:?}"),
                }
                return;
            }
            SessionEvent::Error(e) => format!("connection error: {e}"),
            SessionEvent::Closed => "connection closed".to_owned(),
            SessionEvent::Timeout => "heartbeat timed out".to_owned(),
            SessionEvent::WriterGone => "send failed".to_owned(),
        };
        self.drop_session(&reason);
    }

    /// Answer a session's `Ping` with one of ours, unless something was sent
    /// to it within the last half `heartbeat_interval`.
    ///
    /// This saves wakeups, not correctness: the writer task still pings
    /// after `heartbeat_interval` without a send. Answering here restarts
    /// that timer, so while the receiver keeps pinging, the process wakes up
    /// for the receiver's ping only and not a second time for its own timer.
    fn answer_ping(&mut self) {
        let quiet = self.opts.heartbeat_interval / 2;
        if self
            .session
            .as_ref()
            .is_some_and(|s| s.last_tx.elapsed() >= quiet)
        {
            self.send_to_session(Msg::Ping);
        }
    }

    async fn on_event(&mut self, ev: Event) {
        match ev {
            Event::Session {
                peer,
                mut writer,
                rx,
            } => {
                if self.grab.is_some() {
                    self.release("session replaced");
                }
                if let Some(old) = self.session.take() {
                    info!(id = old.id, peer = %old.peer, "session replaced by a new one");
                }
                let (tx, wrx) = mpsc::channel(SESSION_QUEUE);
                let interval = self.opts.heartbeat_interval;
                let id = self.next_session_id;
                self.next_session_id += 1;
                tokio::spawn(async move {
                    if let Err(e) = session_writer(&mut writer, wrx, interval).await {
                        debug!(id, "session writer stopped: {e}");
                    }
                });
                // The first ping tells the receiver that it is registered.
                // Cannot fail: the queue is empty.
                let _ = tx.try_send(Msg::Ping);
                info!(id, %peer, "session connected");
                self.session = Some(Session {
                    id,
                    peer,
                    tx,
                    rx,
                    last_rx: Instant::now(),
                    last_tx: Instant::now(),
                });
            }
            Event::Toggle { peer, reply } => {
                let outcome = self.toggle().await;
                info!(%peer, ?outcome, "toggle request");
                let _ = reply.send(outcome);
            }
        }
    }

    async fn toggle(&mut self) -> ToggleOutcome {
        if self.grab.is_some() {
            self.release("toggle");
            return ToggleOutcome::Released;
        }
        if self.session.is_none() {
            return ToggleOutcome::NoSession;
        }
        match self.source.grab(self.opts.grab_timeout).await {
            Ok(g) => {
                self.pressed.clear();
                self.grab = Some(g);
                if self.send_to_session(Msg::Grabbed) {
                    info!("keyboard grabbed; forwarding to the receiver");
                    ToggleOutcome::Grabbed
                } else {
                    ToggleOutcome::Failed
                }
            }
            Err(e) => {
                warn!("cannot grab the keyboard: {e:#}");
                ToggleOutcome::Failed
            }
        }
    }

    /// Release the grab (if any) and tell the session.
    fn release(&mut self, reason: &str) {
        if self.grab.take().is_some() {
            self.pressed.clear();
            info!(reason, "keyboard released");
            self.send_to_session(Msg::Released);
        }
    }

    /// Queue a message for the session. On failure the session (and any grab)
    /// is dropped and `false` is returned.
    fn send_to_session(&mut self, msg: Msg) -> bool {
        let Some(s) = &mut self.session else {
            if self.grab.take().is_some() {
                self.pressed.clear();
                warn!("no session; keyboard released");
            }
            return false;
        };
        match s.tx.try_send(msg) {
            Ok(()) => {
                s.last_tx = Instant::now();
                true
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.drop_session("receiver is not reading (send queue full)");
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.drop_session("send failed");
                false
            }
        }
    }

    fn drop_session(&mut self, reason: &str) {
        // Release first so the keyboard is returned even if nothing can be sent.
        let was_grabbed = self.grab.take().is_some();
        if was_grabbed {
            self.pressed.clear();
            info!(reason, "keyboard released");
        }
        if let Some(s) = self.session.take() {
            info!(id = s.id, peer = %s.peer, reason, "session closed");
            if was_grabbed {
                // Best effort; the writer task flushes it and stops once `s.tx` is dropped.
                let _ = s.tx.try_send(Msg::Released);
            }
        }
    }
}

async fn session_writer(
    w: &mut SecureWriter<OwnedWriteHalf>,
    mut rx: mpsc::Receiver<Msg>,
    interval: Duration,
) -> Result<(), crypto::Error> {
    loop {
        let msg = match timeout(interval, rx.recv()).await {
            Ok(Some(m)) => m,
            Ok(None) => return Ok(()),
            Err(_) => Msg::Ping,
        };
        w.send(&msg).await?;
    }
}

struct ConnOptions {
    psk: Psk,
    /// Time allowed for each of `Hello` and `ToggleRequest` to arrive, and for
    /// the toggle reply to be written.
    handshake_wait: Duration,
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    opts: &ConnOptions,
    events: mpsc::Sender<Event>,
    handshake_permit: OwnedSemaphorePermit,
) -> anyhow::Result<()> {
    if let Err(e) = stream.set_nodelay(true) {
        debug!(%peer, "cannot set TCP_NODELAY: {e}");
    }
    let (r, w) = stream.into_split();
    let (mut reader, mut writer) = crypto::accept(r, w, &opts.psk)
        .await
        .context("handshake failed")?;
    let wait = opts.handshake_wait;
    let hello = recv_timeout(&mut reader, wait)
        .await
        .context("waiting for Hello")?;
    // The peer has proven knowledge of the key; stop counting it as pending.
    drop(handshake_permit);
    match hello {
        Msg::Hello {
            role: Role::Session,
        } => {
            let rx = crypto::spawn_reader(reader);
            events
                .send(Event::Session { peer, writer, rx })
                .await
                .map_err(|_| anyhow::anyhow!("server stopped"))?;
        }
        Msg::Hello { role: Role::Toggle } => {
            let deadline = Instant::now() + wait;
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                match recv_timeout(&mut reader, remaining)
                    .await
                    .context("waiting for ToggleRequest")?
                {
                    Msg::ToggleRequest => break,
                    Msg::Ping => continue,
                    other => bail!("unexpected message from toggle client: {other:?}"),
                }
            }
            let (reply, outcome) = oneshot::channel();
            events
                .send(Event::Toggle { peer, reply })
                .await
                .map_err(|_| anyhow::anyhow!("server stopped"))?;
            let outcome = outcome.await.context("server dropped the toggle request")?;
            timeout(wait, writer.send(&Msg::ToggleResult(outcome)))
                .await
                .context("timed out sending ToggleResult")??;
            // Half-close, then read until the client closes. Closing a socket
            // with unread data (e.g. a ping sent while the grab was pending)
            // makes the kernel send RST, which can discard the reply.
            drop(writer);
            let _ = timeout(wait, async { while reader.recv().await.is_ok() {} }).await;
            debug!(%peer, "toggle connection done");
        }
        other => bail!("expected Hello, got {other:?}"),
    }
    Ok(())
}

async fn recv_timeout(
    reader: &mut crypto::SecureReader<OwnedReadHalf>,
    wait: Duration,
) -> anyhow::Result<Msg> {
    Ok(timeout(wait, reader.recv()).await.context("timed out")??)
}

#[cfg(test)]
mod tests;
