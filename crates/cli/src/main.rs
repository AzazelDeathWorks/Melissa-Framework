use melissa_cli::{
    Result,
    app::{App, default_home},
    args::{self, Command},
    display, session,
};
use serde_json::{Value, json};
use std::io::{self, IsTerminal};

async fn run(options: &args::Options) -> Result<Value> {
    if matches!(options.command, Command::Doctor) {
        return melissa_cli::views::doctor();
    }
    if matches!(options.command, Command::Profiles) {
        return Ok(
            json!({"command":"profiles","profiles":melissa_cli::app::profiles()?.profiles,"definition_only":true}),
        );
    }
    let home = options.home.clone().map(Ok).unwrap_or_else(default_home)?;
    if let Command::Session { workspace } = &options.command {
        let workspace = workspace
            .clone()
            .map(Ok)
            .unwrap_or_else(std::env::current_dir)?;
        session::run(
            &home,
            &workspace,
            &mut io::stdin().lock(),
            &mut io::stdout().lock(),
        )
        .await?;
        return Ok(Value::Null);
    }
    if matches!(options.command, Command::Init) {
        return App::init(&home);
    }
    let app = App::open(&home)?;
    match &options.command {
        Command::New {
            workspace,
            goal,
            profile,
            excludes,
            recipe,
        } => {
            if let Some(recipe) = recipe {
                app.create_execution(
                    workspace,
                    goal,
                    excludes.clone(),
                    melissa_cli::recipe::Recipe::read(recipe)?,
                )
            } else {
                app.create(workspace, goal, profile, excludes.clone())
            }
        }
        Command::ConfigImport(path) => app.configure(path),
        Command::Memory {
            workspace,
            action,
            id,
            digest,
        } => app.memory(workspace, action, id.as_deref(), digest.as_deref()),
        Command::Prepare {
            workspace,
            goal,
            out,
            acceptance,
        } => app.prepare(workspace, goal, out, acceptance.as_deref()),
        Command::Replan { id, recipe } => {
            app.replan(id, melissa_cli::recipe::Recipe::read(recipe)?)
        }
        Command::ConfigShow => app.configuration(),
        Command::List => app.list(),
        Command::Show(id) => app.show(id),
        Command::Metrics(id) => app.metrics(id),
        Command::Diff(id) => app.diff(id),
        Command::Events(id) => app.events(id),
        Command::Cancel(id) => app.cancel(id),
        Command::Pause(id) => app.request_control(id, "pause"),
        Command::Run(id) => app.run_task(id, false).await,
        Command::Resume(id) => app.run_task(id, true).await,
        Command::Progress(id) => app.progress(id),
        Command::Logs(id) => app.logs(id),
        Command::DiffContent(id) => app.diff_content(id),
        Command::Delivery { id, out } => app.delivery(id, out.as_deref()),
        Command::Preview { id, out } => app.preview(id, out.as_deref()),
        Command::Wait { id, timeout_ms } => app.wait_task(id, *timeout_ms).await,
        Command::Approve {
            id,
            request,
            digest,
        } => app.approve(id, request, digest),
        Command::Evidence { id, artifact, out } => {
            app.evidence(id, artifact.as_deref(), out.as_deref())
        }
        _ => Err(melissa_cli::Error::Usage("Komut görev akışında değil.")),
    }
}
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
        println!("{}", args::HELP);
        println!("{}", args::SESSION_HELP);
        println!("{}", args::EXECUTION_HELP);
        println!("{}", args::DOMAIN_HELP);
        println!("{}", args::CAPABILITIES_HELP);
        println!("{}", args::METRICS_HELP);
        return;
    }
    if matches!(options.command, Command::Version) {
        println!("melissa {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    match run(&options).await {
        Ok(_) if matches!(options.command, Command::Session { .. }) => {}
        Ok(value) => match if options.json {
            display::json(&value)
        } else {
            display::human(&value)
        } {
            Ok(text) => println!("{text}"),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(4);
            }
        },
        Err(e) => {
            if options.json {
                eprintln!(
                    "{}",
                    display::json(&json!({"ok":false,"error":e.to_string(),"kind":e.kind()}))
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
