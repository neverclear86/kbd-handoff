//! Authenticated, encrypted transport built on the Noise protocol framework
//! (`snow`). No cryptography is implemented here; this module only frames
//! Noise messages over a byte stream.
//!
//! Pattern: `Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s`. Both peers prove
//! knowledge of the 32-byte pre-shared key; a peer without it fails the
//! handshake. Ephemeral keys give each connection fresh session keys.
//!
//! Wire format: every Noise message (handshake or transport) is sent as a
//! big-endian `u16` length followed by that many bytes.

use std::sync::Arc;

use snow::StatelessTransportState;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::config::Psk;
use crate::proto::{DecodeError, Msg};

pub const NOISE_PARAMS: &str = "Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s";
/// Bound into the handshake hash; bump when the wire protocol changes.
const PROLOGUE: &[u8] = b"kbd-handoff/1";
const TAG_LEN: usize = 16;
/// Largest handshake frame accepted. Both `NNpsk0` handshake messages are
/// 48 bytes (ephemeral key + tag of the empty payload).
const MAX_HANDSHAKE_FRAME: usize = 256;
/// Largest transport frame accepted. Every [`Msg`] encodes to at most 4
/// bytes, plus the 16-byte tag; the bound leaves room for protocol growth
/// while keeping a peer from making us buffer much data.
const MAX_TRANSPORT_FRAME: usize = 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("noise error (wrong pre-shared key or corrupted data): {0}")]
    Noise(#[from] snow::Error),
    #[error("malformed message: {0}")]
    Decode(#[from] DecodeError),
    #[error("handshake timed out")]
    HandshakeTimeout,
    #[error("frame too large")]
    FrameTooLarge,
    #[error("nonce space exhausted")]
    NonceExhausted,
}

async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, data: &[u8]) -> Result<(), Error> {
    let len = u16::try_from(data.len()).map_err(|_| Error::FrameTooLarge)?;
    let mut buf = Vec::with_capacity(2 + data.len());
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(data);
    w.write_all(&buf).await?;
    w.flush().await?;
    Ok(())
}

/// Read one frame, rejecting a declared length above `max` before
/// allocating or reading its body.
async fn read_frame<R: AsyncRead + Unpin>(r: &mut R, max: usize) -> Result<Vec<u8>, Error> {
    let mut len = [0u8; 2];
    r.read_exact(&mut len).await?;
    let len = u16::from_be_bytes(len) as usize;
    if len > max {
        return Err(Error::FrameTooLarge);
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

fn builder(psk: &Psk) -> Result<snow::Builder<'_>, Error> {
    let params = NOISE_PARAMS.parse().expect("valid noise params");
    Ok(snow::Builder::new(params)
        .psk(0, psk.as_bytes())?
        .prologue(PROLOGUE)?)
}

/// Receiving half of an encrypted connection.
pub struct SecureReader<R> {
    inner: R,
    state: Arc<StatelessTransportState>,
    nonce: u64,
}

/// Sending half of an encrypted connection.
pub struct SecureWriter<W> {
    inner: W,
    state: Arc<StatelessTransportState>,
    nonce: u64,
}

impl<R: AsyncRead + Unpin> SecureReader<R> {
    /// Read and decrypt one message. Not cancel-safe: drive it from a
    /// dedicated task (see [`spawn_reader`]) rather than inside `select!`.
    pub async fn recv(&mut self) -> Result<Msg, Error> {
        let frame = read_frame(&mut self.inner, MAX_TRANSPORT_FRAME).await?;
        let mut plain = vec![0u8; frame.len()];
        let n = self.state.read_message(self.nonce, &frame, &mut plain)?;
        self.nonce = self.nonce.checked_add(1).ok_or(Error::NonceExhausted)?;
        Ok(Msg::decode(&plain[..n])?)
    }
}

impl<W: AsyncWrite + Unpin> SecureWriter<W> {
    pub async fn send(&mut self, msg: &Msg) -> Result<(), Error> {
        let plain = msg.encode();
        let mut out = vec![0u8; plain.len() + TAG_LEN];
        let n = self.state.write_message(self.nonce, &plain, &mut out)?;
        self.nonce = self.nonce.checked_add(1).ok_or(Error::NonceExhausted)?;
        write_frame(&mut self.inner, &out[..n]).await
    }
}

/// Run the client (initiator) side of the handshake.
pub async fn connect<R, W>(
    mut r: R,
    mut w: W,
    psk: &Psk,
) -> Result<(SecureReader<R>, SecureWriter<W>), Error>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let fut = async {
        let mut hs = builder(psk)?.build_initiator()?;
        let mut buf = [0u8; MAX_HANDSHAKE_FRAME];
        let n = hs.write_message(&[], &mut buf)?;
        write_frame(&mut w, &buf[..n]).await?;
        let frame = read_frame(&mut r, MAX_HANDSHAKE_FRAME).await?;
        hs.read_message(&frame, &mut buf)?;
        Ok::<_, Error>(hs.into_stateless_transport_mode()?)
    };
    let state = tokio::time::timeout(crate::HANDSHAKE_TIMEOUT, fut)
        .await
        .map_err(|_| Error::HandshakeTimeout)??;
    Ok(split(r, w, state))
}

/// Run the server (responder) side of the handshake.
pub async fn accept<R, W>(
    mut r: R,
    mut w: W,
    psk: &Psk,
) -> Result<(SecureReader<R>, SecureWriter<W>), Error>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let fut = async {
        let mut hs = builder(psk)?.build_responder()?;
        let mut buf = [0u8; MAX_HANDSHAKE_FRAME];
        let frame = read_frame(&mut r, MAX_HANDSHAKE_FRAME).await?;
        hs.read_message(&frame, &mut buf)?;
        let n = hs.write_message(&[], &mut buf)?;
        write_frame(&mut w, &buf[..n]).await?;
        Ok::<_, Error>(hs.into_stateless_transport_mode()?)
    };
    let state = tokio::time::timeout(crate::HANDSHAKE_TIMEOUT, fut)
        .await
        .map_err(|_| Error::HandshakeTimeout)??;
    Ok(split(r, w, state))
}

fn split<R, W>(r: R, w: W, state: StatelessTransportState) -> (SecureReader<R>, SecureWriter<W>) {
    let state = Arc::new(state);
    (
        SecureReader {
            inner: r,
            state: state.clone(),
            nonce: 0,
        },
        SecureWriter {
            inner: w,
            state,
            nonce: 0,
        },
    )
}

/// Messages read by a background task started with [`spawn_reader`].
///
/// Dropping this value aborts the task, which closes the read half of the
/// connection even if the peer has gone silent.
pub struct ReaderChannel {
    rx: tokio::sync::mpsc::Receiver<Result<Msg, Error>>,
    task: tokio::task::AbortHandle,
}

impl ReaderChannel {
    /// Receive the next message. Cancel-safe, so it can be used in `select!`.
    /// Returns `None` after the connection has ended and its final error
    /// (if any) has been delivered.
    pub async fn recv(&mut self) -> Option<Result<Msg, Error>> {
        self.rx.recv().await
    }
}

impl Drop for ReaderChannel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Move a reader into a background task and expose its messages through a
/// [`ReaderChannel`], whose `recv()` is cancel-safe and can be used in
/// `select!`. The channel yields one final `Err` (or closes) when the
/// connection ends.
pub fn spawn_reader<R>(mut reader: SecureReader<R>) -> ReaderChannel
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel(256);
    let task = tokio::spawn(async move {
        loop {
            let res = reader.recv().await;
            let stop = res.is_err();
            if tx.send(res).await.is_err() || stop {
                break;
            }
        }
    })
    .abort_handle();
    ReaderChannel { rx, task }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{KeyEvent, KeyValue};

    #[tokio::test]
    async fn handshake_and_exchange() {
        let psk = Psk::from_bytes([7u8; 32]);
        let (a, b) = tokio::io::duplex(4096);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        let psk2 = psk.clone();
        let server = tokio::spawn(async move { accept(br, bw, &psk2).await });
        let (mut cr, mut cw) = connect(ar, aw, &psk).await.unwrap();
        let (mut sr, mut sw) = server.await.unwrap().unwrap();

        let key = Msg::Key(KeyEvent {
            code: 30,
            value: KeyValue::Down,
        });
        sw.send(&key).await.unwrap();
        sw.send(&Msg::Ping).await.unwrap();
        assert_eq!(cr.recv().await.unwrap(), key);
        assert_eq!(cr.recv().await.unwrap(), Msg::Ping);
        cw.send(&Msg::ToggleRequest).await.unwrap();
        assert_eq!(sr.recv().await.unwrap(), Msg::ToggleRequest);
    }

    /// Write only a length header declaring `len` bytes, with no body.
    async fn send_header<W: AsyncWrite + Unpin>(w: &mut W, len: u16) {
        w.write_all(&len.to_be_bytes()).await.unwrap();
        w.flush().await.unwrap();
    }

    #[tokio::test]
    async fn oversize_handshake_frame_rejected() {
        let psk = Psk::from_bytes([7u8; 32]);
        let (a, b) = tokio::io::duplex(4096);
        let (_ar, mut aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        send_header(&mut aw, (MAX_HANDSHAKE_FRAME + 1) as u16).await;
        // Must fail on the header alone, not wait for the body or time out.
        let res = tokio::time::timeout(std::time::Duration::from_secs(1), accept(br, bw, &psk))
            .await
            .expect("oversize frame was not rejected promptly");
        assert!(matches!(res, Err(Error::FrameTooLarge)), "{:?}", res.err());
    }

    #[tokio::test]
    async fn oversize_transport_frame_rejected() {
        let psk = Psk::from_bytes([7u8; 32]);
        let (a, b) = tokio::io::duplex(4096);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        let psk2 = psk.clone();
        let server = tokio::spawn(async move { accept(br, bw, &psk2).await });
        let (_cr, mut cw) = connect(ar, aw, &psk).await.unwrap();
        let (mut sr, _sw) = server.await.unwrap().unwrap();

        // The largest allowed frame is still read (and then fails to decrypt).
        send_header(&mut cw.inner, MAX_TRANSPORT_FRAME as u16).await;
        cw.inner
            .write_all(&[0u8; MAX_TRANSPORT_FRAME])
            .await
            .unwrap();
        let res = sr.recv().await;
        assert!(matches!(res, Err(Error::Noise(_))), "{res:?}");

        send_header(&mut cw.inner, (MAX_TRANSPORT_FRAME + 1) as u16).await;
        let res = tokio::time::timeout(std::time::Duration::from_secs(1), sr.recv())
            .await
            .expect("oversize frame was not rejected promptly");
        assert!(matches!(res, Err(Error::FrameTooLarge)), "{res:?}");
    }

    #[test]
    fn largest_message_fits_transport_frame() {
        let largest = Msg::Key(KeyEvent {
            code: u16::MAX,
            value: KeyValue::Down,
        })
        .encode()
        .len();
        assert!(largest + TAG_LEN <= MAX_TRANSPORT_FRAME);
    }

    #[tokio::test]
    async fn wrong_psk_fails() {
        let (a, b) = tokio::io::duplex(4096);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        let server = tokio::spawn(async move { accept(br, bw, &Psk::from_bytes([1u8; 32])).await });
        let client = connect(ar, aw, &Psk::from_bytes([2u8; 32])).await;
        let server = server.await.unwrap();
        assert!(server.is_err() || client.is_err());
        assert!(
            server.is_err(),
            "responder must reject an initiator with a different PSK"
        );
    }
}
