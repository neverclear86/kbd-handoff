//! Windows receiver: connects to the Linux sender and injects key events.

// A GUI-subsystem executable does not open a console window when a launcher
// or macro device starts it. See `init_logging` for where output goes.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, anyhow, bail};
use kbd_handoff_core::proto::ToggleOutcome;
use kbd_handoff_receiver::{ClientOptions, KeySink, NotGrabbed, ReceiverConfig};

const USAGE: &str = "\
Usage:
  kbd-handoff-receiver toggle [--config PATH]
      Ask the sender to hand the keyboard over or take it back.
      If no receiver session is connected, start `session` in the
      background and exit. Prints the outcome; exits with 0 for
      grabbed/released/session-started, 3 when the sender failed to grab,
      and 1 on other errors.
  kbd-handoff-receiver session [--config PATH] [--dry-run]
      Connect, ask the sender to grab the keyboard, inject the keys it
      forwards, and exit when the keyboard is handed back or the connection
      ends. Exits with 0 after a release, 2 or 3 when the sender did not
      grab (no-session / failed), and 1 on other errors.
  kbd-handoff-receiver run [--config PATH] [--dry-run]
      Stay connected to the sender (reconnecting as needed) and inject the
      keys it forwards whenever another `toggle` grabs the keyboard.
  --dry-run logs key events instead of injecting them (always the case on
  platforms other than Windows).

Default config path: %APPDATA%\\kbd-handoff\\receiver.toml on Windows,
$XDG_CONFIG_HOME/kbd-handoff/receiver.toml (or ~/.config/...) elsewhere.
On Windows, output goes to the console of the invoking terminal if there is
one, and otherwise (and always for `session`) to
%LOCALAPPDATA%\\kbd-handoff\\receiver.log.
Log level: set RUST_LOG (default: info).";

/// Printed by `toggle` when it started a background session.
const SESSION_STARTED: &str = "session-started";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Run {
        config: Option<PathBuf>,
        dry_run: bool,
    },
    Session {
        config: Option<PathBuf>,
        dry_run: bool,
    },
    Toggle {
        config: Option<PathBuf>,
    },
    Help,
}

impl Command {
    /// Whether the command runs in the background without a terminal.
    fn is_background(&self) -> bool {
        matches!(self, Command::Session { .. })
    }
}

fn parse_args(args: impl IntoIterator<Item = String>) -> anyhow::Result<Command> {
    let mut args = args.into_iter();
    let sub = match args.next() {
        None => bail!("missing command"),
        Some(s) => s,
    };
    let mut config = None;
    let mut dry_run = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                let p = args.next().context("--config needs a path")?;
                config = Some(PathBuf::from(p));
            }
            "--dry-run" if sub == "run" || sub == "session" => dry_run = true,
            "-h" | "--help" => return Ok(Command::Help),
            other => bail!("unexpected argument: {other}"),
        }
    }
    match sub.as_str() {
        "run" => Ok(Command::Run { config, dry_run }),
        "session" => Ok(Command::Session { config, dry_run }),
        "toggle" => Ok(Command::Toggle { config }),
        "help" | "-h" | "--help" => Ok(Command::Help),
        other => bail!("unknown command: {other}"),
    }
}

fn default_config_path() -> anyhow::Result<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .context("APPDATA is not set; pass --config")?
    } else {
        match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            Some(v) => PathBuf::from(v),
            None => std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".config"))
                .context("neither XDG_CONFIG_HOME nor HOME is set; pass --config")?,
        }
    };
    Ok(base.join("kbd-handoff").join("receiver.toml"))
}

/// The config path to use, made absolute so that it can be passed on to a
/// background `session` that may run in another directory.
fn config_path(config: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let path = match config {
        Some(p) => p,
        None => default_config_path()?,
    };
    std::path::absolute(&path)
        .with_context(|| format!("cannot resolve config path {}", path.display()))
}

fn load_options(path: &Path) -> anyhow::Result<ClientOptions> {
    // LoadError's message already includes its source, so flatten it to
    // avoid printing the cause twice.
    let cfg: ReceiverConfig = kbd_handoff_core::config::load(path).map_err(|e| anyhow!("{e}"))?;
    Ok(cfg.into_options())
}

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("cannot start the async runtime")
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// Set when logs go to the log file. Results and errors that are normally
/// printed are then logged as well, since there may be no console.
static LOG_TO_FILE: AtomicBool = AtomicBool::new(false);

/// Print a result line to stdout (and to the log file, if used).
fn say(line: &str) {
    println!("{line}");
    if LOG_TO_FILE.load(Ordering::Relaxed) {
        tracing::info!("{line}");
    }
}

/// Print an error to stderr (and to the log file, if used).
fn complain(msg: &str) {
    eprintln!("{msg}");
    if LOG_TO_FILE.load(Ordering::Relaxed) {
        tracing::error!("{msg}");
    }
}

/// Set up `tracing`. Logs go to stderr, except on Windows when there is no
/// console to write to, and always for the background `session`: then they
/// go to `%LOCALAPPDATA%\kbd-handoff\receiver.log` (see [`log_file`]).
///
/// Returns a span that marks file log lines with the process id, because
/// several processes share the file.
fn init_logging(background: bool) -> Option<tracing::span::EnteredSpan> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    match log_file(background) {
        Some(file) => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(file)
                .init();
            LOG_TO_FILE.store(true, Ordering::Relaxed);
            Some(tracing::info_span!("receiver", pid = std::process::id()).entered())
        }
        None => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .init();
            None
        }
    }
}

/// The log file to use instead of stderr, if any.
///
/// A foreground command keeps output the parent redirected to a file or pipe,
/// or attaches to the console of the invoking terminal; only when neither is
/// possible does it use the file. `session` never attaches: a console it
/// shared with the terminal that ran `toggle` would end it when that window
/// closes.
#[cfg(windows)]
fn log_file(background: bool) -> Option<kbd_handoff_receiver::logfile::LogFile> {
    use kbd_handoff_receiver::logfile::{DEFAULT_MAX_LEN, LogFile};
    if !background && console::use_parent_output() {
        return None;
    }
    let base = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty())?;
    let path = PathBuf::from(base).join("kbd-handoff").join("receiver.log");
    LogFile::new(path, DEFAULT_MAX_LEN).ok()
}

#[cfg(not(windows))]
fn log_file(_background: bool) -> Option<kbd_handoff_receiver::logfile::LogFile> {
    None
}

#[cfg(windows)]
mod console {
    use std::os::windows::io::IntoRawHandle;

    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_DISK, FILE_TYPE_PIPE, GetFileType};
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE,
        STD_OUTPUT_HANDLE, SetStdHandle,
    };

    fn std_handle(id: STD_HANDLE) -> Option<HANDLE> {
        // SAFETY: GetStdHandle has no preconditions.
        let h = unsafe { GetStdHandle(id) };
        (!h.is_null() && h != INVALID_HANDLE_VALUE).then_some(h)
    }

    /// Make stdout and stderr reach the invoking terminal. Returns `false`
    /// when there is nothing to write to.
    ///
    /// A GUI-subsystem process starts without a console. If the parent
    /// redirected stderr to a file or pipe, that is kept. Otherwise the
    /// process attaches to the parent's console, if it has one, and points
    /// any missing standard handle at it.
    pub fn use_parent_output() -> bool {
        if let Some(h) = std_handle(STD_ERROR_HANDLE) {
            // SAFETY: `h` is a valid handle returned by GetStdHandle.
            let ty = unsafe { GetFileType(h) };
            if ty == FILE_TYPE_DISK || ty == FILE_TYPE_PIPE {
                return true;
            }
        }
        // SAFETY: no preconditions; fails when the parent has no console.
        if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
            return false;
        }
        for id in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            if std_handle(id).is_some() {
                continue;
            }
            let Ok(out) = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("CONOUT$")
            else {
                continue;
            };
            // The handle is intentionally kept open for the life of the process.
            // SAFETY: `out` is a valid console output handle owned by us.
            unsafe { SetStdHandle(id, out.into_raw_handle() as HANDLE) };
        }
        std_handle(STD_ERROR_HANDLE).is_some()
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

async fn run(opts: ClientOptions, dry_run: bool) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        if !dry_run {
            let mut sink = kbd_handoff_receiver::SendInputSink::new();
            return run_until_shutdown(&opts, &mut sink).await;
        }
    }
    if !dry_run && !cfg!(windows) {
        tracing::info!("not on Windows: key events are logged, not injected");
    }
    run_until_shutdown(&opts, &mut kbd_handoff_receiver::LogSink).await
}

async fn run_until_shutdown<K: KeySink>(opts: &ClientOptions, sink: &mut K) -> anyhow::Result<()> {
    let shutdown = shutdown_or_never();
    tracing::info!(server = %opts.server, "starting receiver");
    // `run_until` drops the session (releasing every held key) before it
    // returns, so the process can exit right after.
    let reason = kbd_handoff_receiver::run_until(opts, sink, shutdown).await?;
    tracing::info!("{reason}; exiting");
    Ok(())
}

async fn session(opts: ClientOptions, dry_run: bool) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        if !dry_run {
            let mut sink = kbd_handoff_receiver::SendInputSink::new();
            return session_until_shutdown(&opts, &mut sink).await;
        }
    }
    if !dry_run && !cfg!(windows) {
        tracing::info!("not on Windows: key events are logged, not injected");
    }
    session_until_shutdown(&opts, &mut kbd_handoff_receiver::LogSink).await
}

async fn session_until_shutdown<K: KeySink>(
    opts: &ClientOptions,
    sink: &mut K,
) -> anyhow::Result<()> {
    let shutdown = shutdown_or_never();
    tracing::info!(server = %opts.server, "starting on-demand session");
    // When `shutdown` wins, `select!` drops the session future before
    // returning; that drop releases the held keys.
    tokio::select! {
        res = kbd_handoff_receiver::run_on_demand(opts, sink) => {
            res?;
            tracing::info!("session ended; exiting");
        }
        reason = shutdown => tracing::info!("{}; exiting", reason?),
    }
    Ok(())
}

/// Start `session` as a detached background process of this executable.
/// Returns its process id.
fn spawn_session(config: &Path) -> anyhow::Result<u32> {
    let exe = std::env::current_exe().context("cannot locate this executable")?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("session")
        .arg("--config")
        .arg(config)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let child = spawn_detached(&mut cmd)
        .with_context(|| format!("cannot start {} session", exe.display()))?;
    Ok(child.id())
}

/// Spawn without a console, in a new process group so that console events
/// for the invoking terminal do not reach it, and outside the caller's job
/// object when the job allows it (a launcher that kills its job on exit
/// would otherwise end the session).
#[cfg(windows)]
fn spawn_detached(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS,
    };
    let flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    match cmd
        .creation_flags(flags | CREATE_BREAKAWAY_FROM_JOB)
        .spawn()
    {
        Ok(child) => Ok(child),
        Err(e) => {
            tracing::debug!("cannot break away from the job ({e}); starting inside it");
            cmd.creation_flags(flags).spawn()
        }
    }
}

/// Spawn in a new process group so that Ctrl-C in the invoking terminal does
/// not reach it.
#[cfg(unix)]
fn spawn_detached(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0).spawn()
}

#[cfg(not(any(windows, unix)))]
fn spawn_detached(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    cmd.spawn()
}

type ShutdownFuture = std::pin::Pin<Box<dyn Future<Output = anyhow::Result<&'static str>>>>;

/// [`shutdown_signal`], or a future that never completes if the handlers
/// cannot be registered (which may happen in a process without a console);
/// failing to register them must not stop the receiver from running.
fn shutdown_or_never() -> ShutdownFuture {
    match shutdown_signal() {
        Ok(f) => Box::pin(f),
        Err(e) => {
            tracing::warn!("{e:#}; console events will not stop this process");
            Box::pin(std::future::pending())
        }
    }
}

/// Register handlers for every console event that ends the process and
/// return a future that completes with a description of the first one.
///
/// On Windows, closing the console window, logging off and shutting down
/// kill the process once the handler returns; tokio keeps the handler
/// blocked, so the process lives until `main` returns (Windows allows a few
/// seconds for this). Windows documents that processes which load
/// `user32.dll` (as `SendInput` does) are not sent the logoff and shutdown
/// events; they are registered anyway in case they arrive. A process without
/// a console (such as a background `session`) receives none of these events.
#[cfg(windows)]
fn shutdown_signal()
-> anyhow::Result<impl std::future::Future<Output = anyhow::Result<&'static str>>> {
    use tokio::signal::windows;
    let mut c = windows::ctrl_c().context("cannot handle Ctrl-C")?;
    let mut brk = windows::ctrl_break().context("cannot handle Ctrl-Break")?;
    let mut close = windows::ctrl_close().context("cannot handle console close")?;
    let mut logoff = windows::ctrl_logoff().context("cannot handle logoff")?;
    let mut shutdown = windows::ctrl_shutdown().context("cannot handle shutdown")?;
    Ok(async move {
        Ok(tokio::select! {
            _ = c.recv() => "interrupted (Ctrl-C)",
            _ = brk.recv() => "interrupted (Ctrl-Break)",
            _ = close.recv() => "console closed",
            _ = logoff.recv() => "user logging off",
            _ = shutdown.recv() => "system shutting down",
        })
    })
}

#[cfg(unix)]
fn shutdown_signal()
-> anyhow::Result<impl std::future::Future<Output = anyhow::Result<&'static str>>> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).context("cannot handle SIGTERM")?;
    let mut int = signal(SignalKind::interrupt()).context("cannot handle SIGINT")?;
    Ok(async move {
        Ok(tokio::select! {
            _ = int.recv() => "interrupted (SIGINT)",
            _ = term.recv() => "terminated (SIGTERM)",
        })
    })
}

#[cfg(not(any(windows, unix)))]
fn shutdown_signal()
-> anyhow::Result<impl std::future::Future<Output = anyhow::Result<&'static str>>> {
    Ok(async {
        tokio::signal::ctrl_c()
            .await
            .context("cannot wait for Ctrl-C")?;
        Ok("interrupted")
    })
}

fn outcome_name(o: ToggleOutcome) -> &'static str {
    match o {
        ToggleOutcome::Grabbed => "grabbed",
        ToggleOutcome::Released => "released",
        ToggleOutcome::NoSession => "no-session",
        ToggleOutcome::Failed => "failed",
    }
}

fn outcome_exit_code(o: ToggleOutcome) -> ExitCode {
    match o {
        ToggleOutcome::Grabbed | ToggleOutcome::Released => ExitCode::SUCCESS,
        ToggleOutcome::NoSession => ExitCode::from(2),
        ToggleOutcome::Failed => ExitCode::from(3),
    }
}

/// Exit code for the result of `session`.
fn session_exit_code(res: &anyhow::Result<()>) -> ExitCode {
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => match e.downcast_ref::<NotGrabbed>() {
            Some(NotGrabbed(o)) => outcome_exit_code(*o),
            None => ExitCode::FAILURE,
        },
    }
}

fn real_main(cmd: Command) -> anyhow::Result<ExitCode> {
    match cmd {
        Command::Help => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        Command::Run { config, dry_run } => {
            let opts = load_options(&config_path(config)?)?;
            runtime()?.block_on(run(opts, dry_run))?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Session { config, dry_run } => {
            let opts = load_options(&config_path(config)?)?;
            let res = runtime()?.block_on(session(opts, dry_run));
            let code = session_exit_code(&res);
            if let Err(e) = res {
                complain(&format!("error: {e:#}"));
            }
            Ok(code)
        }
        Command::Toggle { config } => {
            let path = config_path(config)?;
            let opts = load_options(&path)?;
            let outcome = runtime()?.block_on(kbd_handoff_receiver::toggle(&opts))?;
            if outcome == ToggleOutcome::NoSession {
                let pid = spawn_session(&path)?;
                tracing::info!(pid, "no receiver session; started an on-demand session");
                say(SESSION_STARTED);
                return Ok(ExitCode::SUCCESS);
            }
            say(outcome_name(outcome));
            Ok(outcome_exit_code(outcome))
        }
    }
}

fn main() -> ExitCode {
    let parsed = parse_args(std::env::args().skip(1));
    let background = parsed.as_ref().is_ok_and(Command::is_background);
    let _span = init_logging(background);
    let cmd = match parsed {
        Ok(c) => c,
        Err(e) => {
            complain(&format!("error: {e:#}\n\n{USAGE}"));
            return ExitCode::FAILURE;
        }
    };
    match real_main(cmd) {
        Ok(code) => code,
        Err(e) => {
            complain(&format!("error: {e:#}"));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> anyhow::Result<Command> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_commands() {
        assert_eq!(
            parse(&["run"]).unwrap(),
            Command::Run {
                config: None,
                dry_run: false
            }
        );
        assert_eq!(
            parse(&["run", "--dry-run", "--config", "r.toml"]).unwrap(),
            Command::Run {
                config: Some("r.toml".into()),
                dry_run: true
            }
        );
        assert_eq!(
            parse(&["toggle", "--config", "r.toml"]).unwrap(),
            Command::Toggle {
                config: Some("r.toml".into())
            }
        );
        assert_eq!(
            parse(&["session", "--config", "r.toml", "--dry-run"]).unwrap(),
            Command::Session {
                config: Some("r.toml".into()),
                dry_run: true
            }
        );
        assert!(parse(&["session"]).unwrap().is_background());
        assert!(!parse(&["toggle"]).unwrap().is_background());
        assert!(!parse(&["run"]).unwrap().is_background());
        assert_eq!(parse(&["--help"]).unwrap(), Command::Help);
        assert!(parse(&[]).is_err());
        assert!(parse(&["toggle", "--dry-run"]).is_err());
        assert!(parse(&["run", "--config"]).is_err());
        assert!(parse(&["frobnicate"]).is_err());
    }

    #[test]
    fn exit_codes() {
        assert_eq!(outcome_exit_code(ToggleOutcome::Grabbed), ExitCode::SUCCESS);
        assert_eq!(
            outcome_exit_code(ToggleOutcome::Released),
            ExitCode::SUCCESS
        );
        assert_ne!(
            outcome_exit_code(ToggleOutcome::NoSession),
            ExitCode::SUCCESS
        );
        assert_ne!(outcome_exit_code(ToggleOutcome::Failed), ExitCode::SUCCESS);
    }

    #[test]
    fn session_exit_codes() {
        assert_eq!(session_exit_code(&Ok(())), ExitCode::SUCCESS);
        let failed: anyhow::Result<()> = Err(NotGrabbed(ToggleOutcome::Failed).into());
        assert_eq!(session_exit_code(&failed), ExitCode::from(3));
        let none: anyhow::Result<()> = Err(NotGrabbed(ToggleOutcome::NoSession).into());
        assert_eq!(session_exit_code(&none), ExitCode::from(2));
        let other: anyhow::Result<()> = Err(anyhow!("connection closed"));
        assert_eq!(session_exit_code(&other), ExitCode::FAILURE);
    }

    #[test]
    fn config_path_is_absolute() {
        let p = config_path(Some("r.toml".into())).unwrap();
        assert!(p.is_absolute(), "{}", p.display());
        assert!(p.ends_with("r.toml"));
    }
}
