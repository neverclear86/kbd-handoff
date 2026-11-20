//! Application messages exchanged inside the encrypted channel.
//!
//! Every message is a tag byte followed by a fixed-size payload. Messages are
//! carried one per Noise transport frame (see [`crate::crypto`]).

use crate::input::{KeyEvent, KeyValue};

/// Purpose of a client connection, sent in [`Msg::Hello`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The resident Windows receiver. Receives key events.
    Session,
    /// A short-lived `toggle` command invocation.
    Toggle,
}

/// Result of a toggle request, reported back to the requester.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToggleOutcome {
    /// The keyboard is now grabbed and forwarded to Windows.
    Grabbed,
    /// The keyboard has been returned to Linux.
    Released,
    /// No receiver session is connected, so there is nowhere to forward to.
    NoSession,
    /// Grabbing failed (device missing, permission denied, keys stayed held, ...).
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Msg {
    /// Client -> server. First message after the handshake.
    Hello { role: Role },
    /// Both directions. Keeps the connection alive.
    Ping,
    /// Client -> server. Flip between Linux and Windows.
    ToggleRequest,
    /// Server -> client that sent `ToggleRequest`.
    ToggleResult(ToggleOutcome),
    /// Server -> session. Forwarding has started.
    Grabbed,
    /// Server -> session. Forwarding has stopped; release every held key.
    Released,
    /// Server -> session. A key event to inject.
    Key(KeyEvent),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("empty message")]
    Empty,
    #[error("unknown message tag {0:#04x}")]
    UnknownTag(u8),
    #[error("invalid payload for tag {0:#04x}")]
    BadPayload(u8),
}

const T_HELLO: u8 = 0x01;
const T_PING: u8 = 0x02;
const T_TOGGLE_REQ: u8 = 0x03;
const T_TOGGLE_RES: u8 = 0x04;
const T_GRABBED: u8 = 0x05;
const T_RELEASED: u8 = 0x06;
const T_KEY: u8 = 0x07;

impl Msg {
    pub fn encode(&self) -> Vec<u8> {
        match *self {
            Msg::Hello { role } => vec![
                T_HELLO,
                match role {
                    Role::Session => 0,
                    Role::Toggle => 1,
                },
            ],
            Msg::Ping => vec![T_PING],
            Msg::ToggleRequest => vec![T_TOGGLE_REQ],
            Msg::ToggleResult(o) => vec![
                T_TOGGLE_RES,
                match o {
                    ToggleOutcome::Grabbed => 0,
                    ToggleOutcome::Released => 1,
                    ToggleOutcome::NoSession => 2,
                    ToggleOutcome::Failed => 3,
                },
            ],
            Msg::Grabbed => vec![T_GRABBED],
            Msg::Released => vec![T_RELEASED],
            Msg::Key(ev) => {
                let c = ev.code.to_be_bytes();
                vec![T_KEY, c[0], c[1], ev.value.as_u8()]
            }
        }
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        let (&tag, rest) = buf.split_first().ok_or(DecodeError::Empty)?;
        let bad = || DecodeError::BadPayload(tag);
        let msg = match (tag, rest) {
            (T_HELLO, [0]) => Msg::Hello {
                role: Role::Session,
            },
            (T_HELLO, [1]) => Msg::Hello { role: Role::Toggle },
            (T_PING, []) => Msg::Ping,
            (T_TOGGLE_REQ, []) => Msg::ToggleRequest,
            (T_TOGGLE_RES, [o]) => Msg::ToggleResult(match o {
                0 => ToggleOutcome::Grabbed,
                1 => ToggleOutcome::Released,
                2 => ToggleOutcome::NoSession,
                3 => ToggleOutcome::Failed,
                _ => return Err(bad()),
            }),
            (T_GRABBED, []) => Msg::Grabbed,
            (T_RELEASED, []) => Msg::Released,
            (T_KEY, [a, b, v]) => Msg::Key(KeyEvent {
                code: u16::from_be_bytes([*a, *b]),
                value: KeyValue::from_u8(*v).ok_or_else(bad)?,
            }),
            (
                T_HELLO | T_PING | T_TOGGLE_REQ | T_TOGGLE_RES | T_GRABBED | T_RELEASED | T_KEY,
                _,
            ) => {
                return Err(bad());
            }
            (t, _) => return Err(DecodeError::UnknownTag(t)),
        };
        Ok(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all() {
        let msgs = [
            Msg::Hello {
                role: Role::Session,
            },
            Msg::Hello { role: Role::Toggle },
            Msg::Ping,
            Msg::ToggleRequest,
            Msg::ToggleResult(ToggleOutcome::Grabbed),
            Msg::ToggleResult(ToggleOutcome::Released),
            Msg::ToggleResult(ToggleOutcome::NoSession),
            Msg::ToggleResult(ToggleOutcome::Failed),
            Msg::Grabbed,
            Msg::Released,
            Msg::Key(KeyEvent {
                code: 125,
                value: KeyValue::Down,
            }),
            Msg::Key(KeyEvent {
                code: 0x1ff,
                value: KeyValue::Repeat,
            }),
        ];
        for m in msgs {
            assert_eq!(Msg::decode(&m.encode()), Ok(m));
        }
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(Msg::decode(&[]), Err(DecodeError::Empty));
        assert_eq!(Msg::decode(&[0xff]), Err(DecodeError::UnknownTag(0xff)));
        assert_eq!(
            Msg::decode(&[T_KEY, 0, 1]),
            Err(DecodeError::BadPayload(T_KEY))
        );
        assert_eq!(
            Msg::decode(&[T_KEY, 0, 1, 9]),
            Err(DecodeError::BadPayload(T_KEY))
        );
        assert_eq!(
            Msg::decode(&[T_PING, 0]),
            Err(DecodeError::BadPayload(T_PING))
        );
    }
}
