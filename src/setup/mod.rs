pub mod service;
pub mod thin;
pub mod wizard;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Setup {
        cli: bool,
    },
    Service {
        action: service::Action,
    },
    /// Explicit foreground start (`rustfox run`). Same as bare `rustfox`.
    Run,
}

pub fn parse_args() -> Option<Command> {
    let mut args: Vec<String> = std::env::args().collect();
    args.remove(0);
    parse_args_from(&args)
}

/// Parse CLI tokens after argv[0]. Sets `RUSTFOX_CONFIG_PATH` when a config
/// path is present. Exits the process on invalid `--service` / verb usage.
pub fn parse_args_from(args: &[String]) -> Option<Command> {
    let mut i = 0;
    let mut config_path: Option<String> = None;
    let mut command: Option<Command> = None;

    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                i += 1;
                config_path = args.get(i).cloned();
            }
            "--setup" => {
                let cli = args.get(i + 1).map(|s| s.as_str()) == Some("--cli");
                command = Some(Command::Setup { cli });
                if cli {
                    i += 1;
                }
            }
            "--service" => {
                let action_str = args.get(i + 1).map(|s| s.as_str()).unwrap_or("");
                let action = match parse_service_action(action_str) {
                    Some(a) => a,
                    None => {
                        eprintln!(
                            "Usage: rustfox --service <install|uninstall|remove|status|start|stop|restart>"
                        );
                        eprintln!(
                            "note: `--service` is deprecated; prefer `rustfox <verb>` (e.g. `rustfox install`)."
                        );
                        std::process::exit(1);
                    }
                };
                eprintln!(
                    "note: `rustfox --service …` is deprecated; use `rustfox {}` instead.",
                    action.cli_verb()
                );
                if matches!(action_str, "remove") {
                    eprintln!("note: `remove` is deprecated; use `rustfox uninstall`.");
                }
                command = Some(Command::Service { action });
                i += 1;
            }
            "run" if command.is_none() => {
                command = Some(Command::Run);
            }
            "install" | "uninstall" | "remove" | "status" | "start" | "stop" | "restart"
                if command.is_none() =>
            {
                let action = parse_service_action(args[i].as_str()).expect("matched verb");
                if args[i].as_str() == "remove" {
                    eprintln!("note: `remove` is deprecated; use `rustfox uninstall`.");
                }
                command = Some(Command::Service { action });
            }
            _ => {
                if config_path.is_none() && !args[i].starts_with('-') {
                    config_path = Some(args[i].clone());
                }
            }
        }
        i += 1;
    }

    if let Some(path) = config_path {
        std::env::set_var("RUSTFOX_CONFIG_PATH", path);
    }

    command
}

fn parse_service_action(s: &str) -> Option<service::Action> {
    match s {
        "install" => Some(service::Action::Install),
        "uninstall" | "remove" => Some(service::Action::Uninstall),
        "status" => Some(service::Action::Status),
        "start" => Some(service::Action::Start),
        "stop" => Some(service::Action::Stop),
        "restart" => Some(service::Action::Restart),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn parse_new_verbs() {
        for (verb, want) in [
            ("install", service::Action::Install),
            ("uninstall", service::Action::Uninstall),
            ("remove", service::Action::Uninstall),
            ("status", service::Action::Status),
            ("start", service::Action::Start),
            ("stop", service::Action::Stop),
            ("restart", service::Action::Restart),
        ] {
            assert_eq!(
                parse_args_from(&args(&[verb])),
                Some(Command::Service { action: want }),
                "verb {verb}"
            );
        }
    }

    #[test]
    fn parse_run_alias() {
        assert_eq!(parse_args_from(&args(&["run"])), Some(Command::Run));
        assert_eq!(
            parse_args_from(&args(&["run", "--config", "/tmp/c.toml"])),
            Some(Command::Run)
        );
    }

    #[test]
    fn parse_deprecated_service_flag() {
        assert_eq!(
            parse_args_from(&args(&["--service", "install"])),
            Some(Command::Service {
                action: service::Action::Install
            })
        );
        assert_eq!(
            parse_args_from(&args(&["--service", "remove"])),
            Some(Command::Service {
                action: service::Action::Uninstall
            })
        );
        assert_eq!(
            parse_args_from(&args(&["--service", "restart"])),
            Some(Command::Service {
                action: service::Action::Restart
            })
        );
    }

    #[test]
    fn bare_args_without_verb_is_none() {
        assert_eq!(parse_args_from(&args(&[])), None);
        assert_eq!(parse_args_from(&args(&["--config", "/x.toml"])), None);
    }
}
