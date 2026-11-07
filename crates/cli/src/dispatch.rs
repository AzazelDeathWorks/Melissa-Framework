use crate::{
    Result,
    app::{App, default_home},
    args::{self, Command},
    session,
};
use serde_json::{Value, json};
use std::io::{self, IsTerminal};

pub async fn run(options: &args::Options) -> Result<Value> {
    #[cfg(windows)]
    if let Command::UiRequest { workspace, request } = &options.command {
        let home = options.home.clone().map(Ok).unwrap_or_else(default_home)?;
        let client = crate::frontend::pipe::Client::for_project(&home, workspace)?;
        let request = serde_json::from_str(request)
            .map_err(|_| crate::Error::Usage("Arayüz isteği geçersiz."))?;
        return serde_json::to_value(client.request(request).await?)
            .map_err(|_| crate::Error::Store);
    }
    if matches!(options.command, Command::Doctor) {
        return crate::views::doctor();
    }
    if matches!(options.command, Command::Profiles) {
        return Ok(
            json!({"command":"profiles","profiles":crate::app::profiles()?.profiles,"definition_only":true}),
        );
    }
    if let Command::Maintenance {
        action,
        out,
        source,
        key_file,
        ..
    } = &options.command
    {
        if action == "keygen" {
            return crate::maintenance::portable::keygen(out.as_ref().unwrap());
        }
        if action == "restore" {
            return crate::maintenance::portable::restore(
                source.as_ref().unwrap(),
                out.as_ref().unwrap(),
                key_file.as_ref().unwrap(),
            );
        }
    }
    let home = options.home.clone().map(Ok).unwrap_or_else(default_home)?;
    if matches!(options.command, Command::Configure) {
        if options.json || !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err(crate::Error::Usage(
                "Ayar ekranını etkileşimli terminalde melissa config ile aç. Otomasyon için config show veya config import PATH kullan.",
            ));
        }
        #[cfg(windows)]
        {
            crate::console::settings_dialog(&home).await?;
            return Ok(Value::Null);
        }
        #[cfg(not(windows))]
        return Err(crate::Error::Unavailable(
            "Etkileşimli ayar ekranı Windows sürümünde kullanılabilir.",
        ));
    }
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
        Command::Observe {
            action,
            id,
            out,
            endpoint,
        } => match action.as_str() {
            "diagnose" => app.diagnose(id),
            "support" => app.support(id, out.as_ref().unwrap()),
            _ => {
                app.export_otlp(id, out.as_deref(), endpoint.as_deref())
                    .await
            }
        },
        Command::Maintenance {
            action,
            id,
            out,
            key_file,
            ..
        } => match action.as_str() {
            "disk" => app.disk(id.as_ref().unwrap()),
            "retention-preview" => app.retention_preview(id.as_ref().unwrap()),
            "backup" => app.backup_task(
                id.as_ref().unwrap(),
                out.as_ref().unwrap(),
                key_file.as_ref().unwrap(),
            ),
            _ => Err(crate::Error::Usage("Bakım komutu desteklenmiyor.")),
        },
        Command::Continuity {
            id,
            action,
            request,
            digest,
            label,
        } => match action.as_str() {
            "continuity" | "checkpoints" => app.continuity(id),
            "changes" => crate::continuity::undo::changes(&app, id),
            "checkpoint" => app.checkpoint(id, label.as_ref().unwrap()),
            "adopt-preview" => app.adopt_preview(id),
            "adopt-source" => app.adopt_source(id, digest.as_ref().unwrap()),
            _ => {
                let selection = serde_json::from_value(
                    melissa_model_gateway::strict_json(request.as_ref().unwrap())
                        .map_err(|_| crate::Error::Usage("Geri alma JSON geçersiz."))?,
                )
                .map_err(|_| crate::Error::Usage("Geri alma seçimi geçersiz."))?;
                if action == "undo-preview" {
                    app.undo_preview(id, selection)
                } else {
                    app.undo(id, selection, digest.as_ref().unwrap())
                }
            }
        },
        Command::Services {
            action,
            id,
            workspace,
            path,
            request,
        } => match action.as_str() {
            "configure" => crate::services::settings::configure(
                &app,
                workspace.as_ref().unwrap(),
                path.as_ref().unwrap(),
            ),
            "baseline-accept" => crate::services::frontend::baseline::accept(
                &app,
                id.as_ref().unwrap(),
                serde_json::from_value(
                    melissa_model_gateway::strict_json(request.as_ref().unwrap())
                        .map_err(|_| crate::Error::Usage("Referans JSON geçersiz."))?,
                )
                .map_err(|_| crate::Error::Usage("Referans seçimi geçersiz."))?,
            ),
            "show" => Ok(
                json!({"services":crate::services::settings::load(&app,workspace.as_ref().unwrap())?}),
            ),
            _ => {
                app.run_services(
                    id.as_ref().unwrap(),
                    request
                        .as_ref()
                        .map(|s| {
                            melissa_model_gateway::strict_json(s)
                                .map_err(|_| crate::Error::Usage("Servis JSON geçersiz."))
                                .and_then(|v| {
                                    serde_json::from_value(v)
                                        .map_err(|_| crate::Error::Usage("Servis işlemi geçersiz."))
                                })
                        })
                        .transpose()?,
                )
                .await
            }
        },
        Command::Verification {
            action,
            id,
            workspace,
            path,
            request,
        } => match action.as_str() {
            "configure" => crate::project_verification::settings::configure(
                &app,
                workspace.as_ref().unwrap(),
                path.as_ref().unwrap(),
            ),
            "show" => Ok(
                json!({"verification":crate::project_verification::settings::load(&app,workspace.as_ref().unwrap())?}),
            ),
            _ => {
                app.run_project_verification(
                    id.as_ref().unwrap(),
                    serde_json::from_value(
                        melissa_model_gateway::strict_json(request)
                            .map_err(|_| crate::Error::Usage("Verification JSON geçersiz."))?,
                    )
                    .map_err(|_| crate::Error::Usage("Verification isteği geçersiz."))?,
                    action == "reproduce",
                )
                .await
            }
        },
        Command::RepositoryInspect {
            action,
            workspace,
            query,
        } => crate::repository_inspect::run(&app, action, workspace, query),
        Command::ProjectDoctor { workspace } => {
            app.project_doctor(workspace, app.project_environment(workspace)?)
        }
        Command::Extension {
            action,
            workspace,
            path,
            digest,
            id,
            source,
            query,
        } => match action.as_str() {
            "list" => app.extension_catalog(workspace.as_ref().ok_or(crate::Error::Io)?),
            "configure" => app.configure_extensions(
                workspace.as_ref().ok_or(crate::Error::Io)?,
                path.as_ref().ok_or(crate::Error::Io)?,
            ),
            "import-skill" => {
                app.import_skill(path.as_ref().ok_or(crate::Error::Io)?, digest.as_deref())
            }
            _ => {
                app.resolve_docs(
                    id.as_deref().ok_or(crate::Error::Io)?,
                    source.as_deref().ok_or(crate::Error::Io)?,
                    query.as_deref().ok_or(crate::Error::Io)?,
                )
                .await
            }
        },
        Command::ProjectOperation {
            id,
            operation,
            command,
        } => {
            app.run_project_operation(
                id,
                operation,
                command
                    .as_ref()
                    .map(|s| {
                        serde_json::from_str(s)
                            .map_err(|_| crate::Error::Usage("--command-json geçersiz."))
                    })
                    .transpose()?,
            )
            .await
        }
        Command::ProjectPoll { id, session, after } => {
            app.project_poll(id, session, *after, 32 * 1024)
        }
        Command::ProjectInput {
            id,
            session,
            operation,
            hex,
        } => app.project_input(
            id,
            session,
            &format!(
                "operator:{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| crate::Error::Io)?
                    .as_nanos()
            ),
            operation,
            hex,
        ),
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
                    crate::recipe::Recipe::read(recipe)?,
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
        Command::Replan { id, recipe } => app.replan(id, crate::recipe::Recipe::read(recipe)?),
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
        _ => Err(crate::Error::Usage("Komut görev akışında değil.")),
    }
}
