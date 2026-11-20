//! Minimal `tracing` subscriber that writes one line per event to stderr.
//!
//! The sender stays resident, so it avoids `tracing-subscriber` (and the
//! regex engine its `RUST_LOG` directive parser pulls in): they made up a
//! large part of the binary and of the memory touched at startup. This
//! subscriber only supports a single global level and ignores spans, which
//! the sender does not use.

use std::fmt::{self, Write as _};
use std::io::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::{Event, Metadata, Subscriber, span};

/// Level used when `RUST_LOG` is unset, empty or not a level name.
const DEFAULT_LEVEL: LevelFilter = LevelFilter::INFO;

/// Install the stderr logger. The level comes from `RUST_LOG`, which must be
/// a single level name (`off`, `error`, `warn`, `info`, `debug` or `trace`,
/// in any case); per-target directives such as `some_crate=debug` are not
/// supported.
pub fn init() {
    let var = std::env::var("RUST_LOG").ok();
    let level = var.as_deref().and_then(parse_level);
    let logger = StderrLogger {
        max_level: level.unwrap_or(DEFAULT_LEVEL),
    };
    // Fails only if a global subscriber is already set, which `main` never does.
    let _ = tracing::subscriber::set_global_default(logger);
    if let Some(var) = var.filter(|v| !v.trim().is_empty())
        && level.is_none()
    {
        tracing::warn!(
            "RUST_LOG={var:?} is not a log level (off, error, warn, info, debug, trace); using info"
        );
    }
}

fn parse_level(s: &str) -> Option<LevelFilter> {
    let s = s.trim();
    // `LevelFilter::from_str` also accepts the numbers 0-5; keep to names.
    if s.is_empty() || s.bytes().any(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

struct StderrLogger {
    max_level: LevelFilter,
}

impl Subscriber for StderrLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= self.max_level
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(self.max_level)
    }

    fn event(&self, event: &Event<'_>) {
        let meta = event.metadata();
        let mut line = String::with_capacity(160);
        write_timestamp(&mut line, SystemTime::now());
        let _ = write!(line, " {:>5} {}: ", meta.level(), meta.target());
        let mut visitor = LineVisitor {
            line: &mut line,
            fields: String::new(),
        };
        event.record(&mut visitor);
        let fields = visitor.fields;
        line.push_str(&fields);
        line.push('\n');
        // One write per line, so that lines are not interleaved with other
        // output. Nothing useful can be done if stderr is closed.
        let _ = std::io::stderr().lock().write_all(line.as_bytes());
    }

    // Spans are not used by the sender; treat them as no-ops.
    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }
    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
    fn enter(&self, _: &span::Id) {}
    fn exit(&self, _: &span::Id) {}
}

/// Writes the message directly to the line and collects the other fields as
/// ` name=value`, so that the message comes first whatever the field order.
struct LineVisitor<'a> {
    line: &'a mut String,
    fields: String,
}

impl Visit for LineVisitor<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.line, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
}

/// Append `time` as an RFC 3339 UTC timestamp with microseconds, e.g.
/// `2026-11-20T11:12:20.302237Z`. Times before 1970 are written as the epoch.
fn write_timestamp(out: &mut String, time: SystemTime) {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since_epoch.as_secs();
    let (year, month, day) = civil_from_days(secs / 86_400);
    let secs_of_day = secs % 86_400;
    let _ = write!(
        out,
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:06}Z",
        secs_of_day / 3600,
        secs_of_day / 60 % 60,
        secs_of_day % 60,
        since_epoch.subsec_micros()
    );
}

/// Convert days since 1970-01-01 to a (year, month, day) date in the
/// proleptic Gregorian calendar (Howard Hinnant's `civil_from_days`,
/// restricted to non-negative day counts).
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468; // days since 0000-03-01
    let era = z / 146_097;
    let day_of_era = z % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153; // 0 = March
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn timestamp(secs: u64, micros: u32) -> String {
        let mut s = String::new();
        write_timestamp(&mut s, UNIX_EPOCH + Duration::new(secs, micros * 1000));
        s
    }

    #[test]
    fn formats_timestamps() {
        assert_eq!(timestamp(0, 0), "1970-01-01T00:00:00.000000Z");
        assert_eq!(
            timestamp(951_782_399, 999_999),
            "2000-02-28T23:59:59.999999Z"
        );
        assert_eq!(timestamp(951_782_400, 1), "2000-02-29T00:00:00.000001Z");
        assert_eq!(timestamp(951_868_800, 0), "2000-03-01T00:00:00.000000Z");
        assert_eq!(
            timestamp(1_795_173_140, 302_237),
            "2026-11-20T11:12:20.302237Z"
        );
        assert_eq!(timestamp(4_107_542_399, 0), "2100-02-28T23:59:59.000000Z");
        assert_eq!(timestamp(4_107_542_400, 0), "2100-03-01T00:00:00.000000Z");
    }

    #[test]
    fn parses_levels() {
        assert_eq!(parse_level("debug"), Some(LevelFilter::DEBUG));
        assert_eq!(parse_level(" WARN\n"), Some(LevelFilter::WARN));
        assert_eq!(parse_level("off"), Some(LevelFilter::OFF));
        assert_eq!(parse_level("trace"), Some(LevelFilter::TRACE));
        assert_eq!(parse_level(""), None);
        assert_eq!(parse_level("3"), None);
        assert_eq!(parse_level("kbd_handoff_sender=debug"), None);
        assert_eq!(parse_level("info,tokio=warn"), None);
    }

    #[test]
    fn formats_events_like_the_previous_logger() {
        let mut line = String::new();
        let mut visitor = LineVisitor {
            line: &mut line,
            fields: String::new(),
        };
        // Field values arrive as `Debug`; `%value` fields are wrapped so that
        // their `Debug` output is their `Display` output.
        struct Shown(&'static str);
        impl fmt::Debug for Shown {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.0)
            }
        }
        let callsite_fields = tracing::field::FieldSet::new(
            &["id", "message", "reason"],
            tracing::callsite::Identifier(&CALLSITE),
        );
        let mut fields = callsite_fields.iter();
        visitor.record_debug(&fields.next().unwrap(), &0u64);
        visitor.record_debug(&fields.next().unwrap(), &Shown("session closed"));
        visitor.record_debug(&fields.next().unwrap(), &"send failed");
        let fields = visitor.fields;
        line.push_str(&fields);
        assert_eq!(line, "session closed id=0 reason=\"send failed\"");
    }

    struct TestCallsite;
    static CALLSITE: TestCallsite = TestCallsite;
    impl tracing::callsite::Callsite for TestCallsite {
        fn set_interest(&self, _: tracing::subscriber::Interest) {}
        fn metadata(&self) -> &Metadata<'_> {
            unreachable!("not registered")
        }
    }
}
