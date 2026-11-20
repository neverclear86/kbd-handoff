//! Configuration helpers shared by both binaries.

use std::fmt;
use std::path::Path;

use serde::Deserialize;

/// A 32-byte pre-shared key, written in configuration files as 64 hex digits.
#[derive(Clone, PartialEq, Eq)]
pub struct Psk([u8; 32]);

impl Psk {
    pub fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Generate a fresh random key from the operating system RNG.
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut b = [0u8; 32];
        getrandom::fill(&mut b)?;
        Ok(Self(b))
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn from_hex(s: &str) -> Result<Self, String> {
        let v =
            hex::decode(s.trim()).map_err(|e| format!("pre-shared key is not valid hex: {e}"))?;
        let arr: [u8; 32] = v.try_into().map_err(|v: Vec<u8>| {
            format!(
                "pre-shared key must be 32 bytes (64 hex digits), got {} bytes",
                v.len()
            )
        })?;
        if arr == [0u8; 32] {
            return Err("pre-shared key must not be all zeros".into());
        }
        Ok(Self(arr))
    }
}

impl fmt::Debug for Psk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Psk(<redacted>)")
    }
}

impl<'de> Deserialize<'de> for Psk {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Psk::from_hex(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("cannot read config file {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid config file {path}: {source}")]
    Parse {
        path: String,
        source: toml::de::Error,
    },
}

/// Read and parse a TOML configuration file.
pub fn load<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, LoadError> {
    let p = path.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|source| LoadError::Read {
        path: p.clone(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| LoadError::Parse { path: p, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psk_hex_roundtrip() {
        let k = Psk::generate().unwrap();
        assert_eq!(Psk::from_hex(&k.to_hex()).unwrap(), k);
        assert!(Psk::from_hex("abcd").is_err());
        assert!(Psk::from_hex(&"0".repeat(64)).is_err());
        assert_eq!(format!("{k:?}"), "Psk(<redacted>)");
    }
}
