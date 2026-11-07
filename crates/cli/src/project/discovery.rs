//! Parse project manifests as data; discovery never executes hooks or installs.
use super::{Command, Contract, Environment};
use crate::{Error, Result, app::App};
use melissa_repository::Repository;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::atomic::AtomicBool,
};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Discovery {
    pub source_digest: String,
    pub source_files: usize,
    pub source_bytes: u64,
    pub manifests: BTreeMap<String, String>,
    pub commands: BTreeMap<String, Command>,
    pub sync: Vec<Command>,
    pub requested_versions: BTreeMap<String, String>,
    pub constraints: BTreeMap<String, String>,
    pub issues: Vec<String>,
}
fn file(repo: &Repository, path: &str) -> Result<String> {
    String::from_utf8(repo.read(path)?.1).map_err(|_| Error::Usage("Proje manifesti UTF-8 olmalı."))
}
fn toml_file(repo: &Repository, path: &str) -> Result<toml::Value> {
    toml::from_str(&file(repo, path)?)
        .map_err(|_| Error::Usage("Proje TOML dosyası geçersiz; tahminle komut seçilmedi."))
}
fn command(exe: &str, argv: &[&str], cwd: &str) -> Command {
    Command {
        executable: exe.into(),
        argv: argv.iter().map(|s| s.to_string()).collect(),
        cwd: cwd.into(),
    }
}
fn request(
    versions: &mut BTreeMap<String, String>,
    issues: &mut Vec<String>,
    name: &str,
    value: &str,
    path: &str,
) {
    let name = if name == "nodejs" { "node" } else { name };
    let value = value.trim().trim_start_matches('v');
    if let Some(old) = versions.insert(name.into(), value.into())
        && old != value
    {
        issues.push(format!("Conflicting {name} versions: {old} vs {value} ({path}); select an explicit environment."));
    }
}
pub fn discover(repo: &Repository) -> Result<Discovery> {
    let snapshot = repo.snapshot(&AtomicBool::new(false))?;
    let paths = snapshot
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect::<BTreeSet<_>>();
    let mut d = Discovery {
        source_digest: snapshot.digest.clone(),
        source_files: snapshot.files.len(),
        source_bytes: snapshot.total_bytes,
        manifests: BTreeMap::new(),
        commands: BTreeMap::new(),
        sync: vec![],
        requested_versions: BTreeMap::new(),
        constraints: BTreeMap::new(),
        issues: vec![],
    };
    for f in &snapshot.files {
        let name = f.path.rsplit('/').next().unwrap_or(&f.path);
        let cwd = f.path.rsplit_once('/').map_or(".", |(p, _)| p);
        let prefix = if cwd == "." {
            String::new()
        } else {
            format!("{}-", cwd.replace('/', "-"))
        };
        if matches!(
            name,
            "Cargo.toml"
                | "Cargo.lock"
                | "package.json"
                | "package-lock.json"
                | "pnpm-lock.yaml"
                | "yarn.lock"
                | "pyproject.toml"
                | "uv.lock"
                | "requirements.txt"
                | "rust-toolchain.toml"
                | ".tool-versions"
                | "mise.toml"
                | ".mise.toml"
                | ".python-version"
                | ".node-version"
                | ".nvmrc"
        ) {
            d.manifests.insert(f.path.clone(), f.digest.clone());
        }
        match name {
            "Cargo.toml" => {
                let v = toml_file(repo, &f.path)?;
                if v.get("package").is_some() || v.get("workspace").is_some() {
                    for (id, args) in [
                        ("check", vec!["check", "--locked", "--offline"]),
                        ("build", vec!["build", "--locked", "--offline"]),
                        ("test", vec!["test", "--locked", "--offline"]),
                    ] {
                        d.commands
                            .insert(format!("{prefix}cargo-{id}"), command("cargo", &args, cwd));
                    }
                    if let Some(s) = v
                        .get("package")
                        .and_then(|p| p.get("rust-version"))
                        .and_then(|v| v.as_str())
                    {
                        d.constraints.insert(format!("{cwd}:rust-min"), s.into());
                    }
                }
            }
            "package.json" => {
                let v: Value = serde_json::from_str(&file(repo, &f.path)?)
                    .map_err(|_| Error::Usage("package.json geçersiz."))?;
                let at = |p: &str| {
                    if cwd == "." {
                        p.into()
                    } else {
                        format!("{cwd}/{p}")
                    }
                };
                let locks = [
                    ("npm", "package-lock.json"),
                    ("pnpm", "pnpm-lock.yaml"),
                    ("yarn", "yarn.lock"),
                ]
                .into_iter()
                .filter(|(_, p)| paths.contains(at(p).as_str()))
                .collect::<Vec<_>>();
                let declared = v["packageManager"]
                    .as_str()
                    .map(|s| s.split_once('@').unwrap_or((s, "")));
                let manager = declared
                    .map(|(p, _)| p)
                    .or_else(|| locks.first().map(|(p, _)| *p))
                    .unwrap_or("npm");
                if !["npm", "pnpm", "yarn"].contains(&manager) {
                    d.issues
                        .push(format!("Unsupported package manager {manager}: {}", f.path));
                    continue;
                }
                if (declared.is_none() && locks.len() > 1)
                    || (!locks.is_empty() && !locks.iter().any(|(p, _)| *p == manager))
                {
                    d.issues
                        .push(format!("Package manager/lock conflict: {}", f.path));
                }
                if let Some((p, version)) = declared {
                    let version = version.split('+').next().unwrap_or(version);
                    if !version.is_empty() {
                        request(
                            &mut d.requested_versions,
                            &mut d.issues,
                            p,
                            version,
                            &f.path,
                        );
                    }
                }
                if let Some(engines) = v["engines"].as_object() {
                    for (tool, value) in engines {
                        if let Some(s) = value.as_str() {
                            d.constraints.insert(format!("{cwd}:{tool}"), s.into());
                        }
                    }
                }
                if let Some(scripts) = v["scripts"].as_object() {
                    for id in [
                        "build",
                        "test",
                        "lint",
                        "check",
                        "typecheck",
                        "format:check",
                    ] {
                        if scripts.get(id).is_some_and(Value::is_string) {
                            d.commands.insert(
                                format!("{prefix}{manager}-{}", id.replace(':', "-")),
                                command(manager, &["run", id], cwd),
                            );
                        }
                    }
                }
                if locks.iter().any(|(p, _)| *p == manager) {
                    d.sync.push(match manager {
                        "pnpm" => {
                            command("pnpm", &["install", "--offline", "--frozen-lockfile"], cwd)
                        }
                        "yarn" => {
                            if declared.is_some_and(|(_, v)| v.starts_with("1.")) {
                                command("yarn", &["install", "--offline", "--frozen-lockfile"], cwd)
                            } else {
                                command(
                                    "yarn",
                                    &["install", "--immutable", "--immutable-cache"],
                                    cwd,
                                )
                            }
                        }
                        _ => command("npm", &["ci", "--offline", "--no-audit", "--no-fund"], cwd),
                    });
                }
            }
            "pyproject.toml" => {
                let v = toml_file(repo, &f.path)?;
                if let Some(s) = v
                    .get("project")
                    .and_then(|p| p.get("requires-python"))
                    .and_then(|v| v.as_str())
                {
                    d.constraints.insert(format!("{cwd}:python"), s.into());
                }
                let lock = if cwd == "." {
                    "uv.lock".into()
                } else {
                    format!("{cwd}/uv.lock")
                };
                if paths.contains(lock.as_str()) {
                    d.sync.push(command(
                        "uv",
                        &["sync", "--frozen", "--offline", "--no-python-downloads"],
                        cwd,
                    ));
                    d.commands.insert(
                        format!("{prefix}python-test"),
                        command(
                            "uv",
                            &[
                                "run",
                                "--offline",
                                "--no-sync",
                                "--no-python-downloads",
                                "python",
                                "-m",
                                "pytest",
                            ],
                            cwd,
                        ),
                    );
                } else {
                    d.commands.insert(
                        format!("{prefix}python-test"),
                        command("python", &["-m", "pytest"], cwd),
                    );
                }
            }
            "requirements.txt" => d.sync.push(command(
                "python",
                &[
                    "-m",
                    "pip",
                    "install",
                    "--no-index",
                    "--require-hashes",
                    "-r",
                    "requirements.txt",
                ],
                cwd,
            )),
            ".python-version" if cwd == "." => request(
                &mut d.requested_versions,
                &mut d.issues,
                "python",
                &file(repo, &f.path)?,
                &f.path,
            ),
            ".node-version" | ".nvmrc" if cwd == "." => request(
                &mut d.requested_versions,
                &mut d.issues,
                "node",
                &file(repo, &f.path)?,
                &f.path,
            ),
            "rust-toolchain.toml" if cwd == "." => {
                let v = toml_file(repo, &f.path)?;
                if let Some(s) = v
                    .get("toolchain")
                    .and_then(|p| p.get("channel"))
                    .and_then(|v| v.as_str())
                {
                    request(&mut d.requested_versions, &mut d.issues, "rust", s, &f.path);
                }
            }
            "mise.toml" | ".mise.toml" if cwd == "." => {
                let v = toml_file(repo, &f.path)?;
                if let Some(tools) = v.get("tools").and_then(|v| v.as_table()) {
                    for (tool, v) in tools {
                        if let Some(s) = v.as_str() {
                            request(&mut d.requested_versions, &mut d.issues, tool, s, &f.path);
                        } else {
                            d.issues.push(format!(
                                "Explicit environment required for complex mise selector {tool}."
                            ));
                        }
                    }
                }
            }
            ".tool-versions" if cwd == "." => {
                for line in file(repo, &f.path)?.lines() {
                    let line = line.split('#').next().unwrap_or("");
                    let parts = line.split_whitespace().collect::<Vec<_>>();
                    if parts.len() == 2 {
                        request(
                            &mut d.requested_versions,
                            &mut d.issues,
                            parts[0],
                            parts[1],
                            &f.path,
                        );
                    } else if !parts.is_empty() {
                        d.issues.push(
                            "Multiple .tool-versions selectors need explicit environment choice."
                                .into(),
                        );
                    }
                }
            }
            _ => {}
        }
    }
    if d.commands.is_empty() && paths.iter().any(|p| p.ends_with(".py")) {
        d.commands.insert(
            "python-test".into(),
            command("python", &["-m", "unittest", "discover"], "."),
        );
    }
    repo.validate_snapshot(&snapshot, &AtomicBool::new(false))?;
    Ok(d)
}
impl Discovery {
    pub fn contract(&self, environment: Environment) -> Result<Contract> {
        self.contract_with_checks(environment, None)
    }
    pub fn contract_with_checks(
        &self,
        mut environment: Environment,
        checks: Option<BTreeMap<String, Command>>,
    ) -> Result<Contract> {
        for (k, v) in &self.requested_versions {
            environment
                .versions
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        environment.validate()?;
        if !self.issues.is_empty() {
            return Err(Error::Conflict(
                "Proje ortamında çelişki var; project-doctor ayrıntısını incele.",
            ));
        }
        let checks = checks.unwrap_or_else(|| {
            self.commands
                .iter()
                .filter(|(id, _)| {
                    id.ends_with("test") || id.ends_with("check") || id.ends_with("typecheck")
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        });
        let mut c = Contract {
            services: None,
            verification: None,
            extensions: None,
            environment,
            checks,
            sync: self.sync.clone(),
        };
        let mut spec = crate::project_verification::Spec::default();
        for name in c.checks.keys() {
            let kind = crate::project_verification::check(&c, name);
            if matches!(
                kind.kind,
                crate::project_verification::Kind::Type
                    | crate::project_verification::Kind::Lint
                    | crate::project_verification::Kind::Format
                    | crate::project_verification::Kind::Syntax
            ) {
                spec.after_edit.push(name.clone());
            }
            spec.checks.insert(name.clone(), kind);
        }
        c.verification = Some(spec);
        c.validate()?;
        Ok(c)
    }
}
impl App {
    pub fn project_doctor(&self, workspace: &Path, environment: Environment) -> Result<Value> {
        let repo = Repository::open(
            &crate::app::absolute(workspace)?,
            Default::default(),
            vec![],
        )?;
        self.require_separate_workspace(&repo)?;
        let discovery = discover(&repo)?;
        let config = discovery.contract(environment.clone());
        let transfer_ok = discovery.source_files + 2
            <= melissa_container_executor::MAX_SNAPSHOT_FILES
            && discovery.source_bytes + include_bytes!("worker.py").len() as u64 + 16384
                <= environment
                    .limits
                    .disk_bytes
                    .min(melissa_container_executor::MAX_SNAPSHOT_BYTES);
        Ok(
            json!({"command":"project doctor","discovery":discovery,"environment":environment,"repository_limits":melissa_repository::Limits::default(),"transfer_limits":{"files":melissa_container_executor::MAX_SNAPSHOT_FILES,"bytes":melissa_container_executor::MAX_SNAPSHOT_BYTES,"preflight":if transfer_ok{"PASS"}else{"BLOCKED"}},"runtime_preflight":"NOT_RUN","ready_for_scope_review":config.is_ok() && transfer_ok,"blocker":config.err().map(|e|e.to_string()),"physical_capacity":"inspect with project_process preflight inside the selected worker; configured limits are not detected free RAM","model_calls":0}),
        )
    }
}
