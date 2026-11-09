//! Settings, read from the environment. `compose.yaml` is where users set them.

use std::{net::SocketAddr, path::PathBuf};

use axum::http::HeaderName;

const MIB: u64 = 1024 * 1024;
const DEFAULT_UPLOAD_MB: u64 = 256;
const DEFAULT_QUOTA_MB: u64 = 5000;
const DEFAULT_MIN_FREE_DISK_MB: u64 = 512;
/// This project's app on Hardcover. It's public by design: Hardcover's sign-in with a
/// code needs no secret. `None` until one is registered, and then Hardcover is
/// simply not offered.
const HARDCOVER_CLIENT_ID: Option<&str> = None;
const HARDCOVER_API: &str = "https://api.hardcover.app";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("{0} is required")]
    Missing(&'static str),
    #[error("{name} is invalid: {reason}")]
    Invalid { name: &'static str, reason: String },
}

#[derive(Debug, Clone)]
pub struct Config {
    /// The externally visible origin with no trailing slash. All device URLs
    /// are built from this, never from the request's `Host` header.
    pub public_url: String,
    /// The header carrying the client IP, set only when a trusted proxy
    /// overwrites it. Unset means the socket peer is used.
    pub client_ip_header: Option<HeaderName>,
    pub max_upload_bytes: u64,
    pub default_quota_bytes: u64,
    /// Uploads are refused when less than this is free on the data disk.
    pub min_free_disk_bytes: u64,
    pub kepubify_path: PathBuf,
    pub log_level: String,
    pub data_dir: PathBuf,
    pub site_root: String,
    pub listen_addr: SocketAddr,
    /// Logs redacted Kobo request and response details, for protocol debugging.
    pub kobo_trace: bool,
    /// The Hardcover app users connect through. `None` turns Hardcover off.
    pub hardcover_client_id: Option<String>,
    /// Where Hardcover's API and sign-in are. Only tests change it.
    pub hardcover_api: String,
    /// Non-fatal problems to log once logging is up.
    pub warnings: Vec<String>,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let mut warnings = Vec::new();

        let public_url =
            parse_public_url(&get("PUBLIC_URL").ok_or(ConfigError::Missing("PUBLIC_URL"))?)?;
        if public_url.starts_with("http://") {
            warnings.push("PUBLIC_URL is plain HTTP; use this only on a trusted LAN.".to_owned());
        }

        let client_ip_header =
            match get("CLIENT_IP_HEADER") {
                None => None,
                Some(value) if value.is_empty() => None,
                Some(value) => Some(HeaderName::try_from(value.as_str()).map_err(|e| {
                    ConfigError::Invalid {
                        name: "CLIENT_IP_HEADER",
                        reason: e.to_string(),
                    }
                })?),
            };

        let max_upload_mb = match get("MAX_UPLOAD_MB") {
            None => DEFAULT_UPLOAD_MB,
            Some(value) => parse_positive("MAX_UPLOAD_MB", &value)?,
        };

        let default_quota_mb = match get("DEFAULT_QUOTA_MB") {
            None => DEFAULT_QUOTA_MB,
            Some(value) => parse_positive("DEFAULT_QUOTA_MB", &value)?,
        };

        let min_free_disk_mb = match get("MIN_FREE_DISK_MB") {
            None => DEFAULT_MIN_FREE_DISK_MB,
            Some(value) => parse_positive("MIN_FREE_DISK_MB", &value)?,
        };

        let log_level = get("LOG_LEVEL").unwrap_or_else(|| "info".to_owned());
        if !matches!(
            log_level.as_str(),
            "error" | "warn" | "info" | "debug" | "trace"
        ) {
            return Err(ConfigError::Invalid {
                name: "LOG_LEVEL",
                reason: format!("expected error, warn, info, debug or trace, got {log_level:?}"),
            });
        }

        let listen_addr = get("LISTEN_ADDR")
            .unwrap_or_else(|| "0.0.0.0:3000".to_owned())
            .parse()
            .map_err(|e: std::net::AddrParseError| ConfigError::Invalid {
                name: "LISTEN_ADDR",
                reason: e.to_string(),
            })?;

        let kobo_trace = match get("KOBO_TRACE") {
            None => false,
            Some(value) => parse_bool("KOBO_TRACE", &value)?,
        };

        // Set but empty turns it off, for anyone who doesn't want it offered.
        let hardcover_client_id = match get("HARDCOVER_CLIENT_ID") {
            Some(value) => Some(value.trim().to_owned()).filter(|id| !id.is_empty()),
            None => HARDCOVER_CLIENT_ID.map(str::to_owned),
        };
        if hardcover_client_id.as_deref().is_some_and(|id| {
            id.len() > 100 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        }) {
            return Err(ConfigError::Invalid {
                name: "HARDCOVER_CLIENT_ID",
                reason: "expected an id like 163f8e7c-970a-463d-b6b4-a4e51e4cb4dd".to_owned(),
            });
        }
        let hardcover_api = match get("HARDCOVER_API_URL") {
            None => HARDCOVER_API.to_owned(),
            Some(value) => parse_hardcover_api(&value)?,
        };

        Ok(Self {
            public_url,
            client_ip_header,
            max_upload_bytes: max_upload_mb * MIB,
            default_quota_bytes: default_quota_mb * MIB,
            min_free_disk_bytes: min_free_disk_mb * MIB,
            kepubify_path: get("KEPUBIFY_PATH")
                .map_or_else(|| PathBuf::from("kepubify"), PathBuf::from),
            log_level,
            data_dir: get("DATA_DIR").map_or_else(|| PathBuf::from("data"), PathBuf::from),
            site_root: get("SITE_ROOT").unwrap_or_else(|| "target/site".to_owned()),
            listen_addr,
            kobo_trace,
            hardcover_client_id,
            hardcover_api,
            warnings,
        })
    }
}

/// HTTPS, or plain HTTP to this machine for a test server. The tokens go here.
fn parse_hardcover_api(value: &str) -> Result<String, ConfigError> {
    let value = value.trim_end_matches('/');
    let local = ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"]
        .iter()
        .any(|prefix| value.starts_with(prefix));
    if (value.starts_with("https://") || local) && !value.contains(char::is_whitespace) {
        Ok(value.to_owned())
    } else {
        Err(ConfigError::Invalid {
            name: "HARDCOVER_API_URL",
            reason: "expected https://, or http:// to this machine".to_owned(),
        })
    }
}

fn parse_public_url(value: &str) -> Result<String, ConfigError> {
    let invalid = |reason: &str| ConfigError::Invalid {
        name: "PUBLIC_URL",
        reason: reason.to_owned(),
    };

    let trimmed = value.trim_end_matches('/');
    let Some((scheme, rest)) = trimmed.split_once("://") else {
        return Err(invalid("expected something like https://books.example.com"));
    };
    if !matches!(scheme, "http" | "https") {
        return Err(invalid("the scheme must be http or https"));
    }
    if rest.is_empty() {
        return Err(invalid("the host is missing"));
    }
    if rest.contains(['/', '?', '#', '@'])
        || rest.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(invalid(
            "use only the origin, with no path, query or credentials",
        ));
    }
    Ok(trimmed.to_owned())
}

fn parse_bool(name: &'static str, value: &str) -> Result<bool, ConfigError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(ConfigError::Invalid {
            name,
            reason: format!("expected true or false, got {other:?}"),
        }),
    }
}

fn parse_positive(name: &'static str, value: &str) -> Result<u64, ConfigError> {
    match value.parse::<u64>() {
        Ok(number) if number > 0 && number <= u64::MAX / MIB => Ok(number),
        _ => Err(ConfigError::Invalid {
            name,
            reason: format!("expected a positive number, got {value:?}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Config::from_lookup(|name| map.get(name).cloned())
    }

    const URL: (&str, &str) = ("PUBLIC_URL", "https://books.example.com");

    #[test]
    fn public_url_is_required() {
        assert_eq!(config(&[]).unwrap_err(), ConfigError::Missing("PUBLIC_URL"));
    }

    #[test]
    fn public_url_loses_its_trailing_slash() {
        let c = config(&[("PUBLIC_URL", "https://books.example.com/")]).unwrap();
        assert_eq!(c.public_url, "https://books.example.com");
    }

    #[test]
    fn public_url_rejects_paths_and_other_schemes() {
        for bad in [
            "books.example.com",
            "ftp://x.y",
            "https://x.y/path",
            "https://x.y?a=b",
            "https://",
            "https://u@x.y",
        ] {
            assert!(
                config(&[("PUBLIC_URL", bad)]).is_err(),
                "{bad} should be rejected"
            );
        }
    }

    #[test]
    fn explicit_upload_limit_wins_over_the_default() {
        assert_eq!(config(&[URL]).unwrap().max_upload_bytes, 256 * MIB);
        let c = config(&[URL, ("MAX_UPLOAD_MB", "200")]).unwrap();
        assert_eq!(c.max_upload_bytes, 200 * MIB);
    }

    #[test]
    fn malformed_values_stop_startup() {
        for pair in [
            ("MAX_UPLOAD_MB", "0"),
            ("MAX_UPLOAD_MB", "lots"),
            ("DEFAULT_QUOTA_MB", "-1"),
            ("LOG_LEVEL", "loud"),
            ("CLIENT_IP_HEADER", "not a header"),
            ("LISTEN_ADDR", "nowhere"),
            ("KOBO_TRACE", "1"),
            ("HARDCOVER_CLIENT_ID", "abc def"),
            ("HARDCOVER_CLIENT_ID", "a\nb"),
            ("HARDCOVER_API_URL", "http://api.hardcover.app"),
            ("HARDCOVER_API_URL", "http://127.0.0.1.evil.example:80"),
            ("HARDCOVER_API_URL", "ftp://x"),
        ] {
            assert!(config(&[URL, pair]).is_err(), "{pair:?} should be rejected");
        }
    }

    #[test]
    fn hardcover_can_be_turned_off_or_pointed_at_a_test_server() {
        let off = config(&[URL, ("HARDCOVER_CLIENT_ID", " ")]).unwrap();
        assert_eq!(off.hardcover_client_id, None);
        let on = config(&[
            URL,
            (
                "HARDCOVER_CLIENT_ID",
                "163f8e7c-970a-463d-b6b4-a4e51e4cb4dd",
            ),
            ("HARDCOVER_API_URL", "http://127.0.0.1:4000/"),
        ])
        .unwrap();
        assert_eq!(
            on.hardcover_client_id.as_deref(),
            Some("163f8e7c-970a-463d-b6b4-a4e51e4cb4dd")
        );
        assert_eq!(on.hardcover_api, "http://127.0.0.1:4000");
        assert_eq!(
            config(&[URL]).unwrap().hardcover_api,
            "https://api.hardcover.app"
        );
    }

    #[test]
    fn plain_http_is_allowed_with_a_warning() {
        let c = config(&[("PUBLIC_URL", "http://192.168.1.10:3000")]).unwrap();
        assert_eq!(c.warnings.len(), 1);
    }
}
