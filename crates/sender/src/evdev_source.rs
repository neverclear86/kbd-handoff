//! [`KeySource`] implementation backed by a Linux evdev device.

use std::io;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use evdev::{Device, EventStream, EventSummary, KeyCode};
use kbd_handoff_core::input::{KeyEvent, KeyValue};
use tokio::time::Instant;
use tracing::{debug, warn};

use crate::{GrabbedKeyboard, KeySource};

const KEY_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Grabs the evdev device whose name equals `device_name`.
///
/// The device is looked up by name on every grab, because the
/// `/dev/input/eventN` number can change. While idle no device is open.
#[derive(Debug, Clone)]
pub struct EvdevSource {
    pub device_name: String,
}

impl EvdevSource {
    pub fn new(device_name: impl Into<String>) -> Self {
        Self {
            device_name: device_name.into(),
        }
    }
}

/// A grabbed evdev device. Dropping it ungrabs and closes the device.
pub struct EvdevGrab {
    stream: EventStream,
}

impl KeySource for EvdevSource {
    type Grab = EvdevGrab;

    fn grab(
        &mut self,
        timeout: Duration,
    ) -> impl Future<Output = anyhow::Result<EvdevGrab>> + Send {
        let name = self.device_name.clone();
        async move { grab_by_name(&name, timeout).await }
    }
}

impl GrabbedKeyboard for EvdevGrab {
    async fn next_event(&mut self) -> anyhow::Result<KeyEvent> {
        // Cancel-safe: `EventStream::next_event` only consumes an event when it
        // returns it, and everything after the await is synchronous.
        loop {
            let ev = self
                .stream
                .next_event()
                .await
                .context("reading the input device")?;
            if let EventSummary::Key(_, code, value) = ev.destructure()
                && let Some(value) = KeyValue::from_evdev(value)
            {
                return Ok(KeyEvent {
                    code: code.0,
                    value,
                });
            }
        }
    }
}

fn supports_key_a(dev: &Device) -> bool {
    dev.supported_keys()
        .is_some_and(|k| k.contains(KeyCode::KEY_A))
}

fn open_by_name(name: &str) -> anyhow::Result<Device> {
    let matches: Vec<_> = evdev::enumerate()
        .filter(|(_, dev)| dev.name() == Some(name))
        .map(|(path, dev)| {
            let has_a = supports_key_a(&dev);
            debug!(path = %path.display(), has_a, "found input device with the configured name");
            ((path, dev), has_a)
        })
        .collect();
    let (path, dev) = select_device(name, matches)?;
    debug!(path = %path.display(), "selected input device");
    Ok(dev)
}

/// Choose the device to grab among those whose name equals `name`.
///
/// Each candidate is paired with whether it reports `KEY_A`. A physical
/// keyboard often exposes several nodes with the same name (e.g. one for
/// media keys), so the single node that reports `KEY_A` wins. Without such a
/// node, a lone candidate is used as is. Anything else is ambiguous and an
/// error, rather than grabbing a node that might not carry the letter keys.
fn select_device<T>(name: &str, candidates: Vec<(T, bool)>) -> anyhow::Result<T> {
    let total = candidates.len();
    if total == 0 {
        bail!(
            "input device {name:?} not found (is it connected, and may this user read /dev/input/event*? \
             see `kbd-handoff-sender list-devices`)"
        );
    }
    let with_a = candidates.iter().filter(|(_, a)| *a).count();
    if with_a == 1 {
        return Ok(candidates
            .into_iter()
            .find_map(|(d, a)| a.then_some(d))
            .expect("one candidate reports KEY_A"));
    }
    if with_a == 0 && total == 1 {
        return Ok(candidates.into_iter().next().expect("one candidate").0);
    }
    Err(anyhow!(
        "{total} input devices are named {name:?} and {with_a} of them report KEY_A, \
         so the keyboard to grab is ambiguous; set device_name to a name that only one \
         keyboard has (see `kbd-handoff-sender list-devices`; with keyd, use \"keyd virtual keyboard\")"
    ))
}

fn any_key_held(dev: &Device) -> anyhow::Result<bool> {
    let held = dev.get_key_state().context("reading key state")?;
    Ok(held.iter().next().is_some())
}

/// Read and discard every queued event, returning the codes of the keys
/// released in them. The device must be non-blocking.
fn drain_events(dev: &mut Device) -> io::Result<Vec<u16>> {
    let mut released = Vec::new();
    loop {
        match dev.fetch_events() {
            Ok(events) => {
                released.extend(released_keys(events.filter_map(
                    |ev| match ev.destructure() {
                        EventSummary::Key(_, code, value) => Some((code.0, value)),
                        _ => None,
                    },
                )))
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(released),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Codes of the keys released (`value == 0`) in a sequence of
/// `(code, value)` key events.
fn released_keys(events: impl IntoIterator<Item = (u16, i32)>) -> Vec<u16> {
    events
        .into_iter()
        .filter(|&(_, value)| value == 0)
        .map(|(code, _)| code)
        .collect()
}

async fn grab_by_name(name: &str, timeout: Duration) -> anyhow::Result<EvdevGrab> {
    let mut dev = open_by_name(name)?;
    dev.set_nonblocking(true).context("setting O_NONBLOCK")?;
    let deadline = Instant::now() + timeout;
    loop {
        // Events queued so far have already reached Linux; discard them so
        // that the drain after the grab only sees events from this attempt.
        drain_events(&mut dev).context("draining queued events")?;
        if !any_key_held(&dev)? {
            // Known race (not fixed): a key pressed after the check above
            // but before EVIOCGRAB reaches the compositor as Down; if it is
            // released after EVIOCGRAB, the Up only reaches us, and the
            // compositor keeps the key held until it is pressed again. The
            // window is tiny, but keyd tap/overload keys emit Down and Up in
            // quick succession. An Up in the drain below is the only trace
            // of this, so it is logged as a warning.
            dev.grab()
                .context("grabbing the input device (EVIOCGRAB)")?;
            // Events queued before the grab have already reached Linux; do
            // not replay them to Windows.
            let released = drain_events(&mut dev).context("draining queued events")?;
            // A key pressed between the check and the grab and still held
            // would stay held on the Linux side; give the device back and
            // wait again.
            if !any_key_held(&dev)? {
                if !released.is_empty() {
                    warn!(
                        keys = ?released,
                        "keys were released while grabbing; they may appear stuck on Linux \
                         (press and release them again after returning the keyboard)"
                    );
                }
                break;
            }
            dev.ungrab().context("ungrabbing the input device")?;
        }
        if Instant::now() >= deadline {
            bail!("keys were still held after {timeout:?}; not grabbing");
        }
        tokio::time::sleep(KEY_POLL_INTERVAL).await;
    }
    let stream = dev
        .into_event_stream()
        .context("registering the input device with tokio")?;
    Ok(EvdevGrab { stream })
}

/// Names of input devices that look like keyboards (report `KEY_A` and
/// `KEY_ENTER`), sorted, each with the number of devices of that name that
/// report `KEY_A`. A count above 1 means the name is ambiguous and grabbing
/// by it fails. Devices this user cannot open are omitted.
pub fn list_keyboards() -> Vec<(String, usize)> {
    summarize_keyboards(evdev::enumerate().filter_map(|(_, dev)| {
        let name = dev.name()?.to_owned();
        let keys = dev.supported_keys();
        let has_a = keys.is_some_and(|k| k.contains(KeyCode::KEY_A));
        let has_enter = keys.is_some_and(|k| k.contains(KeyCode::KEY_ENTER));
        Some((name, has_a, has_enter))
    }))
}

/// Group `(name, reports KEY_A, reports KEY_ENTER)` tuples for
/// [`list_keyboards`]. The count uses the same criterion as
/// [`select_device`] (`KEY_A` only).
fn summarize_keyboards(
    devices: impl IntoIterator<Item = (String, bool, bool)>,
) -> Vec<(String, usize)> {
    let mut by_name: std::collections::BTreeMap<String, (usize, bool)> = Default::default();
    for (name, has_a, has_enter) in devices {
        let entry = by_name.entry(name).or_default();
        entry.0 += usize::from(has_a);
        entry.1 |= has_a && has_enter;
    }
    by_name
        .into_iter()
        .filter(|(_, (_, keyboard))| *keyboard)
        .map(|(name, (count, _))| (name, count))
        .collect()
}

/// Parse an evdev key name such as `KEY_LEFTCTRL` into its key code.
pub fn parse_key_name(name: &str) -> Option<u16> {
    KeyCode::from_str(name).ok().map(|k| k.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_the_only_node_with_key_a() {
        assert_eq!(
            select_device("kbd", vec![("a", false), ("b", true)]).unwrap(),
            "b"
        );
        assert_eq!(select_device("kbd", vec![("a", true)]).unwrap(), "a");
        assert_eq!(
            select_device("kbd", vec![("a", false), ("b", true), ("c", false)]).unwrap(),
            "b"
        );
    }

    #[test]
    fn falls_back_to_a_single_match_without_key_a() {
        assert_eq!(select_device("kbd", vec![("a", false)]).unwrap(), "a");
    }

    #[test]
    fn rejects_missing_and_ambiguous_devices() {
        let err = select_device::<&str>("kbd", vec![])
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found"), "{err}");

        let err = select_device("kbd", vec![("a", true), ("b", true), ("c", false)])
            .unwrap_err()
            .to_string();
        assert!(err.contains("3 input devices"), "{err}");
        assert!(err.contains("2 of them report KEY_A"), "{err}");
        assert!(err.contains("keyd virtual keyboard"), "{err}");

        let err = select_device("kbd", vec![("a", false), ("b", false)])
            .unwrap_err()
            .to_string();
        assert!(err.contains("2 input devices"), "{err}");
        assert!(err.contains("0 of them report KEY_A"), "{err}");
    }

    #[test]
    fn summarizes_keyboards_with_counts() {
        let devs = [
            ("Kbd X", true, true),
            ("Kbd X", false, false),
            ("Kbd Y", true, true),
            ("Kbd Y", true, true),
            ("Mouse", false, false),
            ("Odd", true, false),
        ]
        .map(|(n, a, e)| (n.to_owned(), a, e));
        assert_eq!(
            summarize_keyboards(devs),
            vec![("Kbd X".to_owned(), 1), ("Kbd Y".to_owned(), 2)]
        );
    }

    #[test]
    fn collects_released_keys() {
        assert_eq!(
            released_keys([(30, 1), (30, 0), (31, 2), (32, 0), (31, 0)]),
            vec![30, 32, 31]
        );
        assert!(released_keys([(30, 1), (30, 2)]).is_empty());
    }

    #[test]
    fn parses_key_names() {
        assert_eq!(parse_key_name("KEY_LEFTCTRL"), Some(29));
        assert_eq!(parse_key_name("KEY_BACKSPACE"), Some(14));
        assert_eq!(parse_key_name("KEY_HENKAN"), Some(92));
        assert_eq!(parse_key_name("KEY_NOPE"), None);
        assert_eq!(parse_key_name("leftctrl"), None);
    }
}
