//! `kbd-handoff-sender`: the Linux side of kbd-handoff.

#[cfg(target_os = "linux")]
mod logger;
#[cfg(target_os = "linux")]
mod signals;

#[cfg(target_os = "linux")]
mod app {
    use std::net::SocketAddr;
    use std::path::PathBuf;
    use std::process::ExitCode;

    use anyhow::{Context as _, bail};
    use kbd_handoff_core::config::{self, Psk};
    use kbd_handoff_core::{DEFAULT_PORT, HEARTBEAT_INTERVAL, HEARTBEAT_TIMEOUT};
    use kbd_handoff_sender::{
        DEFAULT_EMERGENCY_CHORD, DEFAULT_GRAB_TIMEOUT, EvdevSource, ServerOptions, list_keyboards,
        parse_key_name,
    };
    use serde::Deserialize;
    use tracing::{error, info};

    const USAGE: &str = "\
Usage:
  kbd-handoff-sender run [--config PATH]   Run the sender
  kbd-handoff-sender gen-psk               Print a new random pre-shared key
  kbd-handoff-sender list-devices          List keyboard-like input devices

The default config path is $XDG_CONFIG_HOME/kbd-handoff/sender.toml
(or ~/.config/kbd-handoff/sender.toml). The log level is set with RUST_LOG
(off, error, warn, info, debug or trace; default: info).";

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Config {
        #[serde(default = "default_listen")]
        listen: SocketAddr,
        psk: Psk,
        device_name: String,
        #[serde(default)]
        emergency_release: Option<Vec<String>>,
    }

    fn default_listen() -> SocketAddr {
        SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT))
    }

    fn parse_chord(names: &[String]) -> anyhow::Result<Vec<u16>> {
        if names.is_empty() {
            bail!("emergency_release must contain at least one key");
        }
        names
            .iter()
            .map(|n| {
                parse_key_name(n)
                    .with_context(|| format!("unknown key name in emergency_release: {n:?}"))
            })
            .collect()
    }

    fn default_config_path() -> anyhow::Result<PathBuf> {
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
            return Ok(PathBuf::from(dir).join("kbd-handoff/sender.toml"));
        }
        let home = std::env::var_os("HOME")
            .filter(|d| !d.is_empty())
            .context("neither XDG_CONFIG_HOME nor HOME is set; pass --config")?;
        Ok(PathBuf::from(home).join(".config/kbd-handoff/sender.toml"))
    }

    pub fn main() -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let res = match args.first().map(String::as_str) {
            Some("run") => cmd_run(&args[1..]),
            Some("gen-psk") if args.len() == 1 => cmd_gen_psk(),
            Some("list-devices") if args.len() == 1 => cmd_list_devices(),
            Some("-h" | "--help" | "help") => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        };
        match res {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::FAILURE
            }
        }
    }

    fn cmd_gen_psk() -> anyhow::Result<()> {
        let psk = Psk::generate().map_err(|e| anyhow::anyhow!("cannot get random bytes: {e}"))?;
        println!("{}", psk.to_hex());
        Ok(())
    }

    fn cmd_list_devices() -> anyhow::Result<()> {
        let names = list_keyboards();
        if names.is_empty() {
            eprintln!(
                "no keyboard-like input devices found; this user may lack permission to read /dev/input/event*"
            );
        }
        for (name, count) in names {
            if count > 1 {
                // Tab-separated so the name itself stays easy to copy.
                println!("{name}\t({count} devices share this name; ambiguous)");
            } else {
                println!("{name}");
            }
        }
        Ok(())
    }

    fn cmd_run(args: &[String]) -> anyhow::Result<()> {
        let path = match args {
            [] => default_config_path()?,
            [flag, path] if flag == "--config" => PathBuf::from(path),
            _ => bail!("invalid arguments\n\n{USAGE}"),
        };
        // LoadError already includes its source in Display; avoid printing it twice.
        let cfg: Config = config::load(&path).map_err(|e| anyhow::anyhow!("{e}"))?;
        let chord = match &cfg.emergency_release {
            Some(names) => parse_chord(names)?,
            None => parse_chord(&DEFAULT_EMERGENCY_CHORD.map(String::from))?,
        };

        crate::logger::init();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("cannot start the tokio runtime")?;
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind(cfg.listen)
                .await
                .with_context(|| format!("cannot listen on {}", cfg.listen))?;
            let opts = ServerOptions {
                psk: cfg.psk,
                emergency_chord: chord,
                heartbeat_interval: HEARTBEAT_INTERVAL,
                heartbeat_timeout: HEARTBEAT_TIMEOUT,
                grab_timeout: DEFAULT_GRAB_TIMEOUT,
            };
            info!(device = %cfg.device_name, "starting");
            let source = EvdevSource::new(cfg.device_name);
            let mut signals = crate::signals::ShutdownSignals::new()?;
            // Dropping the `serve` future on a signal releases any grab.
            tokio::select! {
                res = kbd_handoff_sender::serve(listener, source, opts) => {
                    if let Err(e) = &res {
                        error!("server stopped: {e:#}");
                    }
                    res
                }
                signal = signals.recv() => {
                    info!("{} received; exiting", signal?);
                    Ok(())
                }
            }
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn default_chord_parses() {
            assert_eq!(
                parse_chord(&DEFAULT_EMERGENCY_CHORD.map(String::from)).unwrap(),
                vec![29, 56, 42, 14]
            );
            assert!(parse_chord(&[]).is_err());
            assert!(parse_chord(&["KEY_NOPE".to_owned()]).is_err());
        }

        #[test]
        fn example_config_shape() {
            let text = include_str!("../../../sender.example.toml");
            // The placeholder psk is deliberately invalid; substitute a real one.
            let text = text
                .lines()
                .map(|l| {
                    if l.starts_with("psk") {
                        format!("psk = \"{}\"", "ab".repeat(32))
                    } else {
                        l.to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            let dir = std::env::temp_dir()
                .join(format!("kbd-handoff-sender-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("sender.toml");
            std::fs::write(&path, text).unwrap();
            let cfg: Config = config::load(&path).unwrap();
            std::fs::remove_dir_all(&dir).unwrap();
            assert_eq!(cfg.listen, default_listen());
            assert_eq!(cfg.device_name, "keyd virtual keyboard");
            assert_eq!(
                parse_chord(cfg.emergency_release.as_deref().unwrap()).unwrap(),
                vec![29, 56, 42, 14]
            );
        }
    }
}

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    app::main()
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("kbd-handoff-sender only runs on Linux");
    std::process::ExitCode::FAILURE
}
