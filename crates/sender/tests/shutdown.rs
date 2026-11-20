//! Runs the real `kbd-handoff-sender` binary and checks that SIGTERM and
//! SIGINT stop it cleanly.
//!
//! The sender never opens an input device here: that only happens on a
//! toggle request, and none is sent.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

/// Upper bound for the whole test; the sender is killed after this.
const DEADLINE: Duration = Duration::from_secs(20);

fn write_config(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kbd-handoff-sender-shutdown-{}-{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sender.toml");
    let config = format!(
        "listen = \"127.0.0.1:0\"\n\
         psk = \"{}\"\n\
         device_name = \"kbd-handoff test device that does not exist\"\n",
        "ab".repeat(32)
    );
    std::fs::write(&path, config).unwrap();
    path
}

fn stops_cleanly_on(signal: Signal) {
    let name = signal.as_str();
    let config = write_config(name);
    let mut child = Command::new(env!("CARGO_BIN_EXE_kbd-handoff-sender"))
        .arg("run")
        .arg("--config")
        .arg(&config)
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = Pid::from_raw(i32::try_from(child.id()).unwrap());

    // Kill the sender if it does not stop by itself, so that a failure shows
    // up as a failed assertion below instead of a hung test.
    let done = Arc::new(AtomicBool::new(false));
    let watchdog = {
        let done = done.clone();
        std::thread::spawn(move || {
            let start = Instant::now();
            while !done.load(Ordering::Relaxed) {
                if start.elapsed() > DEADLINE {
                    let _ = kill(pid, Signal::SIGKILL);
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        })
    };

    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut log = String::new();
    // The signal handling is in place before the listener is announced.
    while !log.contains("listening") {
        let read = stderr.read_line(&mut log).unwrap();
        assert!(read > 0, "sender exited before listening:\n{log}");
    }
    kill(pid, signal).unwrap();
    stderr.read_to_string(&mut log).unwrap();
    let status = child.wait().unwrap();
    done.store(true, Ordering::Relaxed);
    watchdog.join().unwrap();
    std::fs::remove_dir_all(config.parent().unwrap()).unwrap();

    assert!(status.success(), "exit status {status}:\n{log}");
    assert!(
        log.contains(&format!("{name} received; exiting")),
        "missing shutdown message:\n{log}"
    );
}

#[test]
fn sigterm_stops_the_sender() {
    stops_cleanly_on(Signal::SIGTERM);
}

#[test]
fn sigint_stops_the_sender() {
    stops_cleanly_on(Signal::SIGINT);
}
