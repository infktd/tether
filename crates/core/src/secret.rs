use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;

/// A value that must never appear in logs, errors or panic messages.
///
/// `Debug` and `Display` print `[redacted]`; the value is only reachable
/// through [`Secret::expose`], which makes every use easy to find.
#[derive(Clone)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl<T> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl FromStr for Secret<String> {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_owned()))
    }
}

impl From<String> for Secret<String> {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_display_redact() {
        let secret = Secret::new("hunter2".to_owned());
        assert_eq!(format!("{secret:?}"), "[redacted]");
        assert_eq!(format!("{secret}"), "[redacted]");
        assert_eq!(secret.expose(), "hunter2");
    }

    #[test]
    fn redacts_inside_derived_debug() {
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Config {
            url: Secret<String>,
        }
        let config = Config {
            url: "postgres://u:hunter2@db/x".parse().unwrap(),
        };
        assert!(!format!("{config:?}").contains("hunter2"));
    }
}
