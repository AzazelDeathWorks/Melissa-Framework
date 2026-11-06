use super::Spec;
use crate::{
    Error, Result,
    app::{App, bytes, nonce, safe_path},
};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};
fn slot(app: &App, workspace: &Path) -> Result<(PathBuf, String)> {
    let repo = melissa_repository::Repository::open(
        &crate::app::absolute(workspace)?,
        Default::default(),
        vec![],
    )?;
    let binding = crate::digest(repo.root_identity().as_bytes());
    Ok((
        app.home
            .join("project-services")
            .join(format!("{}.enc", blake3::hash(binding.as_bytes()))),
        binding,
    ))
}
pub fn load(app: &App, workspace: &Path) -> Result<Option<Spec>> {
    let (path, binding) = slot(app, workspace)?;
    safe_path(&path)?;
    if !path.exists() {
        return Ok(None);
    }
    let mut raw = vec![];
    std::fs::File::open(&path)?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut raw)?;
    if raw.len() > 2 * 1024 * 1024 {
        return Err(Error::Store);
    }
    let plain = zeroize::Zeroizing::new(app.key.open(
        &raw,
        format!("melissa.project-services-config/v1:{binding}").as_bytes(),
    )?);
    crate::app::decode(&plain).map(Some)
}
pub fn configure(app: &App, workspace: &Path, path: &Path) -> Result<serde_json::Value> {
    let mut raw = vec![];
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut raw)?;
    if raw.len() > 1024 * 1024 {
        return Err(Error::Usage("Doğrulama ayarı çok büyük."));
    }
    let spec: Spec = crate::app::decode(&raw)?;
    let repo = melissa_repository::Repository::open(
        &crate::app::absolute(workspace)?,
        Default::default(),
        vec![],
    )?;
    app.require_separate_workspace(&repo)?;
    let mut commands = crate::project::discovery::discover(&repo)?.commands;
    for name in &spec.business_checks {
        commands
            .entry(name.clone())
            .or_insert(crate::project::Command {
                executable: "/bin/true".into(),
                argv: vec![],
                cwd: ".".into(),
            });
    }
    let contract = crate::project::Contract {
        services: None,
        verification: None,
        extensions: None,
        environment: Default::default(),
        checks: commands,
        sync: vec![],
    };
    spec.validate(&contract)?;
    if let Some(browser) = &spec.browser {
        super::frontend::baseline::has_baselines(app, workspace, browser)?;
    }
    let (destination, binding) = slot(app, workspace)?;
    std::fs::create_dir_all(destination.parent().ok_or(Error::Io)?)?;
    safe_path(&destination)?;
    let encrypted = app.key.seal(
        &bytes(&spec)?,
        format!("melissa.project-services-config/v1:{binding}").as_bytes(),
    )?;
    let temp = destination.with_extension(format!("{}.tmp", nonce()?));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)?;
    file.write_all(&encrypted)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temp, destination)?;
    Ok(
        serde_json::json!({"command":"services configure","configuration_digest":crate::digest(&bytes(&spec)?),"applies_to":"new_reviewed_tasks_only","model_calls":0}),
    )
}
