//! A small error type.
//!
//! `Server/IMPLEMENTATION.md` §9 pins the dependency set and contains neither
//! `anyhow` nor `thiserror`, so this is hand written. It carries a message and
//! optionally an ICU error code (`PROTOCOL.md` §6).

use std::fmt;

/// Result alias used throughout the server.
pub type Result<T> = std::result::Result<T, Error>;

/// A server error: a message plus an optional protocol error code.
#[derive(Debug)]
pub struct Error {
    msg: String,
    code: Option<u32>,
}

impl Error {
    /// Creates an error from a message.
    pub fn msg(msg: impl fmt::Display) -> Error {
        Error {
            msg: msg.to_string(),
            code: None,
        }
    }

    /// Creates an error carrying a protocol error code.
    pub fn coded(code: u32, msg: impl fmt::Display) -> Error {
        Error {
            msg: msg.to_string(),
            code: Some(code),
        }
    }

    /// The protocol error code, if this error maps onto one.
    pub fn code(&self) -> Option<u32> {
        self.code
    }

    /// Adds context in front of the message.
    pub fn context(self, ctx: impl fmt::Display) -> Error {
        Error {
            msg: format!("{ctx}: {}", self.msg),
            code: self.code,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.code {
            Some(c) => write!(f, "{} [{}]", self.msg, icu_proto::error::name(c)),
            None => f.write_str(&self.msg),
        }
    }
}

impl std::error::Error for Error {}

macro_rules! from_display {
    ($($t:ty),* $(,)?) => {
        $(impl From<$t> for Error {
            fn from(e: $t) -> Error {
                Error::msg(e)
            }
        })*
    };
}

from_display!(
    std::io::Error,
    std::net::AddrParseError,
    std::num::ParseIntError,
    std::string::FromUtf8Error,
    toml::de::Error,
    toml::ser::Error,
    serde_json::Error,
    rcgen::Error,
    rustls::Error,
    quinn::ConnectionError,
    quinn::ReadExactError,
    quinn::ReadToEndError,
    quinn::WriteError,
    quinn::ClosedStream,
    quinn::ConnectError,
    quinn::crypto::rustls::NoInitialCipherSuite,
    rustls::server::VerifierBuilderError,
    icu_proto::ProtoError,
    icu_proto::NameError,
    icu_proto::VersionError,
    time::error::Format,
    time::error::Parse,
    rustix::io::Errno,
);

/// Adds context to a `Result`.
pub trait Context<T> {
    /// Prefixes any error with `ctx`.
    fn context(self, ctx: impl fmt::Display) -> Result<T>;
}

impl<T, E> Context<T> for std::result::Result<T, E>
where
    E: Into<Error>,
{
    fn context(self, ctx: impl fmt::Display) -> Result<T> {
        self.map_err(|e| e.into().context(ctx))
    }
}

impl<T> Context<T> for Option<T> {
    fn context(self, ctx: impl fmt::Display) -> Result<T> {
        self.ok_or_else(|| Error::msg(ctx))
    }
}
