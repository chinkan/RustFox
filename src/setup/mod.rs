pub mod service;
pub mod wizard;

pub enum Command {
    Setup { cli: bool },
    Service { action: service::Action },
}

pub fn parse_args() -> Option<Command> {
    let mut args: Vec<String> = std::env::args().collect();
    args.remove(0);

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
                let action = match action_str {
                    "install" => service::Action::Install,
                    "remove" => service::Action::Remove,
                    "status" => service::Action::Status,
                    "start" => service::Action::Start,
                    "stop" => service::Action::Stop,
                    _ => {
                        eprintln!("Usage: rustfox --service <install|remove|status|start|stop>");
                        std::process::exit(1);
                    }
                };
                command = Some(Command::Service { action });
                i += 1;
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
