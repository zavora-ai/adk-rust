//! The shell's command line (spec 3.3 §5.5), parsed without ever repeating a value it does not
//! know: an argument it ignores may be a token passed under a mistaken flag.

/// What stands in the log for an ignored argument that is not a flag.
const A_VALUE: &str = "<value>";

/// `adk-desktop`'s command line: `--server`, `--token`, `--probe` and `--help`, with
/// `--name=value` accepted for the first two.
///
/// Arguments it does not know are ignored, since a WebDriver host may add its own. They are
/// kept for the log by name only: a flag up to any `=`, anything else as `<value>`, so a token
/// given as `--auth-token 3f9c…`, `--auth-token=3f9c…` or `-t 3f9c…` never reaches the log.
///
/// # Example
///
/// ```
/// use adk_desktop_core::CommandLine;
///
/// let line = CommandLine::parse(["--server", "127.0.0.1:8792", "--auth-token", "s3cret"]).unwrap();
/// assert_eq!(line.server.as_deref(), Some("127.0.0.1:8792"));
/// assert_eq!(line.ignored, ["--auth-token", "<value>"]);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandLine {
    /// `--server`.
    pub server: Option<String>,
    /// `--token`.
    pub token: Option<String>,
    /// `--probe`.
    pub probe: bool,
    /// `-h` or `--help`.
    pub help: bool,
    /// The arguments it ignored, safe to log: flag names and `<value>`.
    pub ignored: Vec<String>,
}

impl CommandLine {
    /// Parses the arguments after the program's name.
    ///
    /// # Errors
    ///
    /// Returns what is missing when `--server` or `--token` ends the line without its value.
    pub fn parse<I, S>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut line = Self::default();
        let mut args = args.into_iter().map(Into::into);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--server" => line.server = Some(args.next().ok_or("--server needs an address")?),
                "--token" => line.token = Some(args.next().ok_or("--token needs a value")?),
                "--probe" => line.probe = true,
                "-h" | "--help" => line.help = true,
                other => {
                    if let Some(server) = other.strip_prefix("--server=") {
                        line.server = Some(server.to_owned());
                    } else if let Some(token) = other.strip_prefix("--token=") {
                        line.token = Some(token.to_owned());
                    } else if other.starts_with('-') && other.len() > 1 {
                        let name = other.split_once('=').map_or(other, |(name, _)| name);
                        line.ignored.push(name.to_owned());
                    } else {
                        line.ignored.push(A_VALUE.to_owned());
                    }
                }
            }
        }
        Ok(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_flags_in_both_forms() {
        let line =
            CommandLine::parse(["--server=http://127.0.0.1:8792", "--token", "t0ken", "--probe"])
                .unwrap();
        assert_eq!(
            line,
            CommandLine {
                server: Some("http://127.0.0.1:8792".into()),
                token: Some("t0ken".into()),
                probe: true,
                help: false,
                ignored: vec![],
            }
        );
        assert!(CommandLine::parse(["--help"]).unwrap().help);
        assert_eq!(CommandLine::parse(["--token"]).unwrap_err(), "--token needs a value");
    }

    #[test]
    fn an_ignored_argument_never_carries_its_value_to_the_log() {
        let secret = "3f9c0a7e5b";
        let joined = format!("--auth-token={secret}");
        let line = CommandLine::parse([
            "--auth-token",
            secret,
            joined.as_str(),
            "-t",
            secret,
            secret,
            "-",
        ])
        .unwrap();
        assert_eq!(
            line.ignored,
            ["--auth-token", "<value>", "--auth-token", "-t", "<value>", "<value>", "<value>"]
        );
        assert!(!format!("{line:?}").contains(secret), "{line:?}");
        assert_eq!(line.token, None);
    }
}
