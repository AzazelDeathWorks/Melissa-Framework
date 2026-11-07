use melissa_cli::{
    args::{self, Command},
    display,
};
use serde_json::json;
use std::io::{self, IsTerminal};

#[tokio::main]
async fn main() {
    let mut cli_args = std::env::args_os().skip(1).peekable();
    let parsed =
        if cli_args.peek().is_none() && io::stdin().is_terminal() && io::stdout().is_terminal() {
            args::parse(["session".into()])
        } else {
            args::parse(cli_args)
        };
    let options = match parsed {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    if matches!(options.command, Command::Help) {
        println!(
            "Melissa CLI\n\n  melissa                  Doğrudan sohbet aç\n  melissa config           Sağlayıcı ve model ayarlarını aç\n  melissa session --workspace PATH\n\nİsteğini doğrudan yaz. Plan için /plan veya Shift+Tab.\nSohbet içinde: Ayarlar /settings · Model /model · Geçmiş /resume · /help\n\nTeknik otomasyon komutları: melissa --help-all"
        );
        return;
    }
    if matches!(options.command, Command::HelpAll) {
        println!("{}", args::HELP);
        println!("{}", args::SESSION_HELP);
        println!("{}", args::PROJECT_HELP);
        println!("{}", args::EXTENSION_HELP);
        println!("{}", args::EXECUTION_HELP);
        println!("{}", args::DOMAIN_HELP);
        println!("{}", args::CAPABILITIES_HELP);
        println!("{}", args::METRICS_HELP);
        println!("{}", args::REPOSITORY_HELP);
        println!("{}", args::VERIFICATION_HELP);
        println!("{}", args::SERVICES_HELP);
        println!("{}", args::CONTINUITY_HELP);
        println!("{}", args::OBSERVABILITY_HELP);
        return;
    }
    if matches!(options.command, Command::Version) {
        println!("melissa {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    match melissa_cli::dispatch::run(&options).await {
        Ok(_)
            if matches!(
                options.command,
                Command::Session { .. } | Command::Configure
            ) => {}
        Ok(value) => match if options.json {
            display::json(&value)
        } else {
            display::human(&value)
        } {
            Ok(text) => {
                println!("{text}");
                if matches!(&options.command, Command::Verification { action, .. } if action == "verify" || action == "reproduce")
                    && value["result"]["operation_status"] != "PASS"
                {
                    std::process::exit(7);
                }
                if matches!(&options.command,Command::Observe{action,..} if action=="otlp")
                    && value["status"] != "PASS"
                {
                    std::process::exit(7);
                }
                if matches!(&options.command,Command::Continuity{action,..} if action=="undo")
                    && value["outcome"] != "PASS"
                {
                    std::process::exit(7);
                }
                if matches!(&options.command,Command::Services{action,..} if action=="run"||action=="session")
                    && value["outcome"] != "PASS"
                {
                    std::process::exit(7);
                }
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(4);
            }
        },
        Err(e) => {
            if options.json {
                eprintln!(
                    "{}",
                    display::json(&json!({"ok":false,"error":e.to_string(),"kind":e.kind(),"diagnosis":melissa_cli::diagnostics::explain(&e)}))
                        .unwrap_or_else(|_| "{\"ok\":false}".into())
                );
            } else {
                eprintln!("{e}");
            }
            std::process::exit(match e {
                melissa_cli::Error::Usage(_) => 2,
                melissa_cli::Error::Conflict(_) => 3,
                melissa_cli::Error::Unavailable(_) => 5,
                melissa_cli::Error::Reconciliation(_) => 6,
                _ => 4,
            });
        }
    }
}
