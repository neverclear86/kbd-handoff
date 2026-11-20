//! SIGINT and SIGTERM handling through `signalfd`.
//!
//! tokio's `signal` feature is deliberately not used. Its driver allocates
//! state for every signal number when the runtime starts and keeps a socket
//! pair open. That measured 20 to 40 kB of private memory for the life of
//! the process, which matters when the whole idle footprint is about 200 kB.

use std::io;

use anyhow::Context as _;
use nix::sys::signal::{SigSet, Signal};
use nix::sys::signalfd::{SfdFlags, SignalFd};
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;

/// Receives SIGINT and SIGTERM.
pub struct ShutdownSignals {
    fd: AsyncFd<SignalFd>,
}

impl ShutdownSignals {
    /// Block SIGINT and SIGTERM and deliver them through a signalfd instead.
    ///
    /// Must be called inside a tokio runtime, on the main thread, before any
    /// other thread is started (threads inherit the signal mask).
    pub fn new() -> anyhow::Result<Self> {
        let mut mask = SigSet::empty();
        mask.add(Signal::SIGINT);
        mask.add(Signal::SIGTERM);
        // Errors are converted to `io::Error` so that nix's tables of errno
        // names and descriptions are not linked in.
        //
        // Block last: if anything before it fails, the signals keep their
        // default action and still terminate the process.
        let fd = SignalFd::with_flags(&mask, SfdFlags::SFD_NONBLOCK | SfdFlags::SFD_CLOEXEC)
            .map_err(io::Error::from)
            .context("cannot create the signalfd")?;
        // SAFETY: `SignalFd` owns its file descriptor, and the `AsyncFd`
        // owns the `SignalFd`, so the descriptor stays open and is not
        // replaced for as long as it is registered.
        let fd = unsafe { AsyncFd::register_with_interest(fd, Interest::READABLE) }
            .map_err(io::Error::from)
            .context("cannot register the signalfd with tokio")?;
        mask.thread_block()
            .map_err(io::Error::from)
            .context("cannot block SIGINT and SIGTERM")?;
        Ok(Self { fd })
    }

    /// Wait for the next signal and return its name. Cancel-safe.
    pub async fn recv(&mut self) -> anyhow::Result<&'static str> {
        loop {
            let mut ready = self
                .fd
                .readable()
                .await
                .context("waiting on the signalfd")?;
            match ready.get_inner().read_signal() {
                Ok(Some(info)) => {
                    // Only the two signals in the mask can arrive.
                    return Ok(if info.ssi_signo == Signal::SIGINT as u32 {
                        "SIGINT"
                    } else {
                        "SIGTERM"
                    });
                }
                Ok(None) => ready.clear_ready(),
                Err(e) => return Err(io::Error::from(e)).context("reading the signalfd"),
            }
        }
    }
}
