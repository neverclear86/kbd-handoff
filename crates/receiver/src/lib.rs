//! Windows receiver: connects to the Linux sender and injects key events.
//!
//! The session logic is written against the [`KeySink`] trait so that it can
//! run and be tested on any platform. [`LogSink`] only logs events;
//! `SendInputSink` (Windows only) injects them with `SendInput`.
//!
//! Two ways of running a session are provided: [`run_on_demand`] for one
//! session that requests the grab itself and ends when the keyboard is
//! handed back (the `session` command), and [`run_forever`] / [`run_until`]
//! for a resident receiver that reconnects (the `run` command).

use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use kbd_handoff_core::config::Psk;
use kbd_handoff_core::crypto::{self, SecureWriter};
use kbd_handoff_core::input::KeyValue;
use kbd_handoff_core::proto::{Msg, Role, ToggleOutcome};
use serde::Deserialize;
use tokio::io::AsyncWrite;
use tokio::net::TcpStream;
use tokio::time::{Instant, MissedTickBehavior};

/// Upper bound for establishing the TCP connection to the sender.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound for a whole `toggle` request, from connecting to receiving
/// the result. The sender may wait up to 3 seconds for keys to be released
/// before it grabs the keyboard.
pub const TOGGLE_TIMEOUT: Duration = Duration::from_secs(10);
/// Default initial reconnect delay.
pub const RECONNECT_MIN: Duration = Duration::from_secs(1);
/// Default maximum reconnect delay.
pub const RECONNECT_MAX: Duration = Duration::from_secs(10);

/// Smallest ping period accepted; `tokio::time::interval` panics on zero.
const MIN_PING_PERIOD: Duration = Duration::from_millis(1);

/// Destination for key events received from the sender.
pub trait KeySink: Send {
    /// Inject one key event identified by its evdev key code.
    fn inject(&mut self, code: u16, value: KeyValue) -> anyhow::Result<()>;
}

impl<K: KeySink + ?Sized> KeySink for &mut K {
    fn inject(&mut self, code: u16, value: KeyValue) -> anyhow::Result<()> {
        (**self).inject(code, value)
    }
}

impl<K: KeySink + ?Sized> KeySink for Box<K> {
    fn inject(&mut self, code: u16, value: KeyValue) -> anyhow::Result<()> {
        (**self).inject(code, value)
    }
}

/// Wraps a sink and remembers which keys are currently pressed, so that they
/// can all be released when forwarding stops or the connection is lost.
///
/// `Down` and `Repeat` mark a key as held, `Up` clears it. Held keys are also
/// released when the wrapper is dropped (for example when the session future
/// is cancelled).
pub struct HeldKeys<K: KeySink> {
    sink: K,
    held: BTreeSet<u16>,
}

impl<K: KeySink> HeldKeys<K> {
    pub fn new(sink: K) -> Self {
        Self {
            sink,
            held: BTreeSet::new(),
        }
    }

    /// Send `Up` for every held key, in ascending key code order.
    ///
    /// Every key is attempted and the held set is cleared even if the sink
    /// fails; the first error is returned.
    pub fn release_all(&mut self) -> anyhow::Result<()> {
        let mut first_err = None;
        for code in std::mem::take(&mut self.held) {
            if let Err(e) = self.sink.inject(code, KeyValue::Up) {
                first_err.get_or_insert(e);
            }
        }
        match first_err {
            Some(e) => Err(e.context("failed to release held keys")),
            None => Ok(()),
        }
    }

    /// Key codes currently held, in ascending order.
    pub fn held(&self) -> impl Iterator<Item = u16> + '_ {
        self.held.iter().copied()
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    pub fn sink(&self) -> &K {
        &self.sink
    }

    pub fn sink_mut(&mut self) -> &mut K {
        &mut self.sink
    }
}

impl<K: KeySink> KeySink for HeldKeys<K> {
    fn inject(&mut self, code: u16, value: KeyValue) -> anyhow::Result<()> {
        match value {
            KeyValue::Down | KeyValue::Repeat => {
                self.held.insert(code);
            }
            KeyValue::Up => {
                self.held.remove(&code);
            }
        }
        self.sink.inject(code, value)
    }
}

impl<K: KeySink> Drop for HeldKeys<K> {
    fn drop(&mut self) {
        if !self.held.is_empty()
            && let Err(e) = self.release_all()
        {
            tracing::warn!("{e:#}");
        }
    }
}

/// A sink that only logs events. Used on non-Windows platforms and for
/// `--dry-run`.
#[derive(Debug, Default, Clone, Copy)]
pub struct LogSink;

impl KeySink for LogSink {
    fn inject(&mut self, code: u16, value: KeyValue) -> anyhow::Result<()> {
        tracing::info!(code, ?value, "key");
        Ok(())
    }
}

#[cfg(windows)]
pub use windows_sink::SendInputSink;

#[cfg(windows)]
mod windows_sink {
    use kbd_handoff_core::input::KeyValue;
    use kbd_handoff_core::keymap::evdev_to_scancode;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
        KEYEVENTF_SCANCODE, SendInput,
    };

    use super::KeySink;

    /// Injects key events into Windows with `SendInput`, using scan codes.
    #[derive(Debug, Default)]
    pub struct SendInputSink;

    impl SendInputSink {
        pub fn new() -> Self {
            Self
        }
    }

    impl KeySink for SendInputSink {
        fn inject(&mut self, code: u16, value: KeyValue) -> anyhow::Result<()> {
            let Some(sc) = evdev_to_scancode(code) else {
                tracing::debug!(code, ?value, "no scan code for evdev key; skipped");
                return Ok(());
            };
            let mut flags = KEYEVENTF_SCANCODE;
            if sc.extended {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            // Repeat is injected as another key-down, because keys pressed
            // through SendInput do not auto-repeat on Windows.
            if value == KeyValue::Up {
                flags |= KEYEVENTF_KEYUP;
            }
            let input = INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: 0,
                        wScan: sc.code,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            // SAFETY: `input` is a fully initialised INPUT that lives for the
            // duration of the call, and the count and size match it.
            let sent = unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) };
            if sent != 1 {
                let err = std::io::Error::last_os_error();
                anyhow::bail!("SendInput failed for evdev key {code} ({value:?}): {err}");
            }
            Ok(())
        }
    }
}

/// Connection settings for the receiver and the `toggle` command.
#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// Sender address as `host:port`.
    pub server: String,
    pub psk: Psk,
    /// How often to send `Ping`.
    pub heartbeat_interval: Duration,
    /// The connection is considered dead when no frame arrives within this time.
    pub heartbeat_timeout: Duration,
    /// First reconnect delay; also used after a session that was established.
    pub reconnect_min: Duration,
    /// Reconnect delays double up to this value.
    pub reconnect_max: Duration,
}

impl ClientOptions {
    /// Options with the protocol's default timings.
    pub fn new(server: impl Into<String>, psk: Psk) -> Self {
        Self {
            server: server.into(),
            psk,
            heartbeat_interval: kbd_handoff_core::HEARTBEAT_INTERVAL,
            heartbeat_timeout: kbd_handoff_core::HEARTBEAT_TIMEOUT,
            reconnect_min: RECONNECT_MIN,
            reconnect_max: RECONNECT_MAX,
        }
    }
}

/// Contents of `receiver.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiverConfig {
    /// Sender address as `host:port`, e.g. `192.0.2.10:47820`.
    pub server: String,
    pub psk: Psk,
}

impl ReceiverConfig {
    pub fn into_options(self) -> ClientOptions {
        ClientOptions::new(self.server, self.psk)
    }
}

type Writer = SecureWriter<tokio::net::tcp::OwnedWriteHalf>;
type Reader = crypto::SecureReader<tokio::net::tcp::OwnedReadHalf>;

/// Open a TCP connection to the sender and run the Noise handshake.
async fn connect(opts: &ClientOptions) -> anyhow::Result<(Reader, Writer)> {
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&opts.server))
        .await
        .map_err(|_| anyhow!("timed out connecting to {}", opts.server))?
        .with_context(|| format!("cannot connect to {}", opts.server))?;
    if let Err(e) = stream.set_nodelay(true) {
        tracing::debug!("set_nodelay failed: {e}");
    }
    let (r, w) = stream.into_split();
    crypto::connect(r, w, &opts.psk)
        .await
        .with_context(|| format!("handshake with {} failed", opts.server))
}

async fn send<W: AsyncWrite + Unpin>(w: &mut SecureWriter<W>, msg: &Msg) -> anyhow::Result<()> {
    w.send(msg)
        .await
        .with_context(|| format!("failed to send {msg:?}"))
}

fn ping_interval(period: Duration) -> tokio::time::Interval {
    let period = period.max(MIN_PING_PERIOD);
    let mut iv = tokio::time::interval_at(Instant::now() + period, period);
    iv.set_missed_tick_behavior(MissedTickBehavior::Delay);
    iv
}

/// Run one receiver session: connect, handshake, announce `Role::Session`,
/// then inject key events until the connection ends.
///
/// Returns `Err` on every exit: connection failure, handshake failure,
/// disconnect, heartbeat timeout, protocol error or sink error. All keys
/// pressed through this session are released before returning.
pub async fn run_session<K: KeySink>(opts: &ClientOptions, sink: &mut K) -> anyhow::Result<()> {
    let mut established = false;
    session(opts, sink, &mut established).await
}

async fn session<K: KeySink>(
    opts: &ClientOptions,
    sink: &mut K,
    established: &mut bool,
) -> anyhow::Result<()> {
    let (reader, mut writer) = connect(opts).await?;
    send(
        &mut writer,
        &Msg::Hello {
            role: Role::Session,
        },
    )
    .await?;
    *established = true;
    tracing::info!(server = %opts.server, "session established");

    let mut held = HeldKeys::new(sink);
    let res = session_loop(opts, reader, &mut writer, &mut held).await;
    finish(res, held.release_all())
}

/// Combine a session result with the result of releasing its held keys,
/// preferring the session's error.
fn finish(res: anyhow::Result<()>, released: anyhow::Result<()>) -> anyhow::Result<()> {
    match (res, released) {
        (Err(e), Err(re)) => {
            tracing::warn!("{re:#}");
            Err(e)
        }
        (Err(e), Ok(())) => Err(e),
        (Ok(()), Err(re)) => Err(re),
        (Ok(()), Ok(())) => Ok(()),
    }
}

async fn session_loop<K: KeySink>(
    opts: &ClientOptions,
    reader: Reader,
    writer: &mut Writer,
    held: &mut HeldKeys<K>,
) -> anyhow::Result<()> {
    let mut rx = crypto::spawn_reader(reader);
    let mut ping = ping_interval(opts.heartbeat_interval);
    let deadline = tokio::time::sleep(opts.heartbeat_timeout);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            msg = rx.recv() => {
                let msg = match msg {
                    Some(Ok(msg)) => msg,
                    Some(Err(e)) => return Err(anyhow::Error::new(e).context("connection lost")),
                    None => bail!("connection closed"),
                };
                deadline.as_mut().reset(Instant::now() + opts.heartbeat_timeout);
                match msg {
                    Msg::Ping => {}
                    Msg::Key(ev) => held.inject(ev.code, ev.value)?,
                    Msg::Grabbed => tracing::info!("keyboard grabbed; forwarding to this machine"),
                    Msg::Released => {
                        tracing::info!("keyboard released; returned to the sender");
                        held.release_all()?;
                    }
                    other => tracing::warn!(?other, "ignoring unexpected message"),
                }
            }
            _ = ping.tick() => send(writer, &Msg::Ping).await?,
            () = &mut deadline => {
                bail!("no message from the sender within {:?}", opts.heartbeat_timeout)
            }
        }
    }
}

/// Error returned by [`run_on_demand`] when the sender answers the session's
/// own toggle request with something other than [`ToggleOutcome::Grabbed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotGrabbed(pub ToggleOutcome);

impl std::fmt::Display for NotGrabbed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the sender did not hand over the keyboard (outcome: {:?})",
            self.0
        )
    }
}

impl std::error::Error for NotGrabbed {}

/// Run one on-demand session: connect as `Role::Session`, ask the sender to
/// grab the keyboard as soon as the session is registered, inject the
/// forwarded keys, and return once the keyboard is handed back.
///
/// The session is registered when the sender's first `Ping` arrives; the
/// `ToggleRequest` is sent on that session's own connection.
///
/// Returns `Ok(())` when `Msg::Released` arrives (a toggle from elsewhere,
/// the emergency chord, or any other release on the sender). Returns
/// [`NotGrabbed`] (inside `anyhow::Error`) when the toggle result is not
/// `Grabbed`, and another error on connection failure, disconnect, heartbeat
/// timeout, a missing toggle result, a protocol error or a sink error. There
/// is no reconnect. All keys pressed through this session are released
/// before returning, and also when the future is dropped.
pub async fn run_on_demand<K: KeySink>(opts: &ClientOptions, sink: &mut K) -> anyhow::Result<()> {
    let (reader, mut writer) = connect(opts).await?;
    send(
        &mut writer,
        &Msg::Hello {
            role: Role::Session,
        },
    )
    .await?;
    tracing::info!(server = %opts.server, "on-demand session connected");
    let mut held = HeldKeys::new(sink);
    let res = on_demand_loop(opts, reader, &mut writer, &mut held).await;
    finish(res, held.release_all())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnDemandPhase {
    /// Waiting for the first `Ping`, which means the sender registered us.
    Registering,
    /// `ToggleRequest` sent; waiting for its `ToggleResult`.
    Requested,
    /// The keyboard is forwarded to this session.
    Forwarding,
}

async fn sleep_until_opt(t: Option<Instant>) {
    match t {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}

async fn on_demand_loop<K: KeySink>(
    opts: &ClientOptions,
    reader: Reader,
    writer: &mut Writer,
    held: &mut HeldKeys<K>,
) -> anyhow::Result<()> {
    let mut rx = crypto::spawn_reader(reader);
    let mut ping = ping_interval(opts.heartbeat_interval);
    let deadline = tokio::time::sleep(opts.heartbeat_timeout);
    tokio::pin!(deadline);
    let mut phase = OnDemandPhase::Registering;
    // The sender keeps pinging while it waits to grab, so the heartbeat
    // alone would not notice a toggle result that never comes.
    let mut result_deadline: Option<Instant> = None;

    loop {
        tokio::select! {
            msg = rx.recv() => {
                let msg = match msg {
                    Some(Ok(msg)) => msg,
                    Some(Err(e)) => return Err(anyhow::Error::new(e).context("connection lost")),
                    None => bail!("connection closed"),
                };
                deadline.as_mut().reset(Instant::now() + opts.heartbeat_timeout);
                match msg {
                    Msg::Ping => {
                        if phase == OnDemandPhase::Registering {
                            send(writer, &Msg::ToggleRequest).await?;
                            phase = OnDemandPhase::Requested;
                            result_deadline = Some(Instant::now() + TOGGLE_TIMEOUT);
                            tracing::debug!("registered; toggle requested");
                        }
                    }
                    Msg::ToggleResult(outcome) if phase == OnDemandPhase::Requested => {
                        if outcome != ToggleOutcome::Grabbed {
                            return Err(NotGrabbed(outcome).into());
                        }
                        phase = OnDemandPhase::Forwarding;
                        result_deadline = None;
                        tracing::info!("keyboard grabbed; forwarding to this machine");
                    }
                    Msg::Key(ev) => held.inject(ev.code, ev.value)?,
                    Msg::Grabbed => tracing::debug!("sender reports the keyboard grabbed"),
                    Msg::Released => {
                        tracing::info!("keyboard released; returned to the sender");
                        return Ok(());
                    }
                    other => tracing::warn!(?other, "ignoring unexpected message"),
                }
            }
            _ = ping.tick() => send(writer, &Msg::Ping).await?,
            () = &mut deadline => {
                bail!("no message from the sender within {:?}", opts.heartbeat_timeout)
            }
            () = sleep_until_opt(result_deadline) => {
                bail!("no toggle result from the sender within {TOGGLE_TIMEOUT:?}")
            }
        }
    }
}

/// Delay to use after `current` when the previous attempt failed.
fn next_delay(current: Duration, max: Duration) -> Duration {
    current.saturating_mul(2).min(max)
}

/// Run sessions forever, reconnecting with exponential backoff between
/// `reconnect_min` and `reconnect_max`. The backoff starts over after a
/// session that was established.
pub async fn run_forever<K: KeySink>(opts: &ClientOptions, sink: &mut K) -> ! {
    let max = opts.reconnect_max.max(opts.reconnect_min);
    let mut delay = opts.reconnect_min;
    loop {
        let mut established = false;
        let res = session(opts, sink, &mut established).await;
        if let Err(e) = res {
            if established {
                tracing::warn!("session ended: {e:#}");
            } else {
                tracing::warn!("connection failed: {e:#}");
            }
        }
        if established {
            delay = opts.reconnect_min;
        }
        tracing::info!("reconnecting in {delay:?}");
        tokio::time::sleep(delay).await;
        if !established {
            delay = next_delay(delay, max);
        }
    }
}

/// Run [`run_forever`] until `shutdown` completes, then stop.
///
/// The session future is dropped before this returns, which releases every
/// key it pressed (see [`HeldKeys`]), so a caller may exit the process as
/// soon as this returns.
pub async fn run_until<K: KeySink, F: Future>(
    opts: &ClientOptions,
    sink: &mut K,
    shutdown: F,
) -> F::Output {
    // `select!` drops the session future when `shutdown` wins, before
    // returning; that drop releases the held keys synchronously.
    tokio::select! {
        _ = run_forever(opts, sink) => unreachable!(),
        out = shutdown => out,
    }
}

/// Ask the sender to flip the keyboard between Linux and Windows.
///
/// Connects directly to the sender with `Role::Toggle`, independent of any
/// running receiver session, and waits at most [`TOGGLE_TIMEOUT`] for the
/// result.
pub async fn toggle(opts: &ClientOptions) -> anyhow::Result<ToggleOutcome> {
    tokio::time::timeout(TOGGLE_TIMEOUT, toggle_inner(opts))
        .await
        .map_err(|_| anyhow!("no toggle result from the sender within {TOGGLE_TIMEOUT:?}"))?
}

async fn toggle_inner(opts: &ClientOptions) -> anyhow::Result<ToggleOutcome> {
    let (reader, mut writer) = connect(opts).await?;
    send(&mut writer, &Msg::Hello { role: Role::Toggle }).await?;
    send(&mut writer, &Msg::ToggleRequest).await?;
    let mut rx = crypto::spawn_reader(reader);
    // Keep pinging while the sender waits for keys to be released.
    let mut ping = ping_interval(opts.heartbeat_interval);
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Some(Ok(Msg::ToggleResult(outcome))) => return Ok(outcome),
                Some(Ok(Msg::Ping)) => {}
                Some(Ok(other)) => tracing::debug!(?other, "ignoring unexpected message"),
                Some(Err(e)) => {
                    return Err(anyhow::Error::new(e).context("connection lost before the toggle result"));
                }
                None => bail!("connection closed before the toggle result"),
            },
            _ = ping.tick() => send(&mut writer, &Msg::Ping).await?,
        }
    }
}

pub mod logfile;

#[cfg(test)]
mod tests;
