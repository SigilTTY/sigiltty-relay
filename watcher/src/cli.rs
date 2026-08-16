//! Command line surface (docs/PROTOCOL.md §10). Pure parsing, so every
//! shape the bootstrap can send is a unit test rather than a deployment.
//!
//! Two of these commands exist so the binary can answer questions the app
//! used to answer from side files — the version marker it wrote at install
//! time, the PID in the lock file, `sed` over the config. A binary that
//! describes itself cannot go stale the way those could.
//!
//! Backward compatibility is the constraint on the grammar: deployed install
//! scripts run `sigiltty-watcher --config <path>` with no command word at
//! all, so a bare invocation must keep meaning "run the watch loop", and
//! `--version` must keep printing the bare semver it has printed since 0.1.0.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// The watch loop — the default, and what the bootstrap starts.
    Run(PathBuf),
    /// One key=value line describing this deployment (§10).
    Status(PathBuf),
    Uninstall { config: PathBuf, keep_binary: bool },
    Version,
    Help,
}

pub const USAGE: &str = "\
sigiltty-watcher — server-side herdr agent watcher for SigilTTY offline push

USAGE:
    sigiltty-watcher [--config <path>]              watch (default)
    sigiltty-watcher status [--config <path>]       report this deployment
    sigiltty-watcher uninstall [--config <path>] [--keep-binary]
    sigiltty-watcher --version
    sigiltty-watcher --help

OPTIONS:
    --config <path>   config file (default: $XDG_CONFIG_HOME/sigiltty/relay.json)
    --keep-binary     uninstall: remove the config and runtime state, but leave
                      the binary installed so re-enabling costs no download

Uninstall stops the running watcher, then removes the config, the lock file,
the log, the legacy version marker and (unless --keep-binary) the installed
binary, plus any directory those leave empty.";

/// The command word may come before or after `--config`; both orders are
/// written by hand often enough that rejecting one is just a papercut.
pub fn parse<I: Iterator<Item = String>>(
    args: I,
    default_config: PathBuf,
) -> Result<Command, String> {
    let mut args = args.peekable();
    let mut config: Option<PathBuf> = None;
    let mut keep_binary = false;
    let mut command: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            // Terminal flags: nothing after them can change the answer.
            "--version" | "-V" => return Ok(Command::Version),
            "--help" | "-h" => return Ok(Command::Help),
            "--config" => {
                let Some(path) = args.next() else {
                    return Err("--config requires a path".into());
                };
                config = Some(path.into());
            }
            "--keep-binary" => keep_binary = true,
            other if other.starts_with('-') => {
                return Err(format!("unknown option: {other}"));
            }
            other => {
                if let Some(first) = &command {
                    return Err(format!("unexpected argument: {other} (already have {first})"));
                }
                command = Some(other.to_string());
            }
        }
    }

    let config = config.unwrap_or(default_config);
    match command.as_deref() {
        None | Some("run") => Ok(Command::Run(config)),
        Some("status") => Ok(Command::Status(config)),
        Some("uninstall") => Ok(Command::Uninstall { config, keep_binary }),
        Some("version") => Ok(Command::Version),
        Some("help") => Ok(Command::Help),
        Some(other) => Err(format!("unknown command: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(|s| s.to_string()), PathBuf::from("/default/relay.json"))
    }

    /// The shape every deployed install script sends. If this ever breaks,
    /// every server in the field stops watching on its next restart.
    #[test]
    fn the_deployed_invocations_keep_their_meaning() {
        assert_eq!(parse_args(&[]), Ok(Command::Run("/default/relay.json".into())));
        assert_eq!(
            parse_args(&["--config", "/etc/relay.json"]),
            Ok(Command::Run("/etc/relay.json".into()))
        );
        assert_eq!(parse_args(&["--version"]), Ok(Command::Version));
    }

    #[test]
    fn commands_take_the_config_on_either_side() {
        let expected = Command::Status("/etc/relay.json".into());
        assert_eq!(parse_args(&["status", "--config", "/etc/relay.json"]), Ok(expected.clone()));
        assert_eq!(parse_args(&["--config", "/etc/relay.json", "status"]), Ok(expected));
        assert_eq!(parse_args(&["status"]), Ok(Command::Status("/default/relay.json".into())));
    }

    #[test]
    fn uninstall_carries_its_one_flag() {
        assert_eq!(
            parse_args(&["uninstall"]),
            Ok(Command::Uninstall { config: "/default/relay.json".into(), keep_binary: false })
        );
        assert_eq!(
            parse_args(&["uninstall", "--keep-binary", "--config", "/etc/relay.json"]),
            Ok(Command::Uninstall { config: "/etc/relay.json".into(), keep_binary: true })
        );
    }

    #[test]
    fn both_spellings_of_the_two_terminal_flags() {
        assert_eq!(parse_args(&["-V"]), Ok(Command::Version));
        assert_eq!(parse_args(&["version"]), Ok(Command::Version));
        assert_eq!(parse_args(&["--help"]), Ok(Command::Help));
        assert_eq!(parse_args(&["-h"]), Ok(Command::Help));
        assert_eq!(parse_args(&["help"]), Ok(Command::Help));
        // Terminal: a typo after them is not worth failing a --version probe,
        // which is exactly what the bootstrap runs to decide about upgrading.
        assert_eq!(parse_args(&["--version", "--nonsense"]), Ok(Command::Version));
    }

    #[test]
    fn mistakes_are_refused_rather_than_guessed() {
        assert!(parse_args(&["--config"]).is_err());
        assert!(parse_args(&["--nonsense"]).is_err());
        assert!(parse_args(&["uninstal"]).is_err());
        assert!(parse_args(&["status", "uninstall"]).is_err());
    }
}
