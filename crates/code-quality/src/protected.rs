//! Protected black-box QA. Expected answers stay on the host; candidate code
//! runs only through the existing isolated CODE-03 executor, without mounts,
//! networking, API credentials, protected test files or host execution fallback.
use crate::{Error, Result, bounded, digest, profiles::ReviewProfiles, source::FrozenSource};
use melissa_container_executor::{
    ContainerLimits, ContainerReport, ContainerRequest, Executor, Snapshot, TerminationReason,
};
use melissa_local_store::{ArtifactRequest, LocalStore};
use melissa_repository::{Limits, Repository, archive::inspect_ustar};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicBool, Ordering},
};
use zeroize::Zeroizing;

pub const SDK_IMAGE: &str =
    "sha256:fd18bccfbb336754152a58054354bcea114919c38c07ab40a25ced856f613c1e";
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Rust,
    RustCargo,
    Python,
    JavaScript,
}
/// Host-only choices, never a model-provided command or container definition.
pub struct ProgramSpec {
    pub language: Language,
    pub entry: String,
}
pub struct Case {
    pub id: String,
    pub input: Value,
    pub expected: Value,
}
pub struct Suite {
    cases: Vec<Case>,
    digest: String,
}
impl Suite {
    pub fn new(cases: Vec<Case>) -> Result<Self> {
        let mut ids = BTreeSet::new();
        let mut inputs = BTreeSet::new();
        if cases.len() < 2 || cases.len() > 128 {
            return Err(Error::Invalid("independent suite requires 2..128 cases"));
        }
        let mut hasher = blake3::Hasher::new();
        for c in &cases {
            let bytes = serde_json::to_vec(&(&c.id, &c.input, &c.expected))?;
            if !bounded(&c.id, 128)
                || !ids.insert(c.id.clone())
                || !inputs.insert(c.input.to_string())
                || bytes.len() > 32768
            {
                return Err(Error::Invalid("unique bounded protected cases required"));
            }
            hasher.update(&(bytes.len() as u64).to_le_bytes());
            hasher.update(&bytes);
        }
        Ok(Self {
            cases,
            digest: format!("blake3:{}", hasher.finalize()),
        })
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
}
/// Immutable in-memory result; no Deserialize or public constructor/fields.
pub struct BuiltProgram {
    source: String,
    environment: String,
    package: BTreeMap<String, Vec<u8>>,
    executable: String,
    arguments: Vec<String>,
    build_log: String,
}
impl BuiltProgram {
    pub fn source_digest(&self) -> &str {
        &self.source
    }
    pub fn package(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.package
    }
    pub fn package_digest(&self) -> Result<String> {
        Ok(digest(&serde_json::to_vec(&self.package)?))
    }
    pub fn environment_digest(&self) -> &str {
        &self.environment
    }
}
pub struct QaConfig {
    pub task_id: String,
    pub resource_id: String,
    pub authorization_ref: String,
    pub run_id: String,
    pub expires_at_ms: u64,
    pub limits: ContainerLimits,
}
pub struct ProtectedQa {
    config: QaConfig,
    store: LocalStore,
    profiles: ReviewProfiles,
    key: Zeroizing<[u8; 32]>,
    revoked: bool,
    sequence: u64,
}
#[derive(Debug, Serialize)]
pub struct QaReceipt {
    source_digest: String,
    package_digest: String,
    environment_digest: String,
    suite_digest: String,
    verifier_digest: String,
    log_digest: String,
    passed: u64,
    total: u64,
    negative_control_rejected: bool,
    run_id: String,
    task_id: String,
    mac: String,
}
#[derive(Serialize)]
pub struct StaticEvidence {
    source_digest: String,
    rules_digest: String,
    image: String,
    version: String,
    raw_log_digest: String,
}
#[derive(Serialize)]
pub struct ReleaseAcceptance {
    schema_version: &'static str,
    source_digest: String,
    package_digest: String,
    qa_receipt_digest: String,
    security_digest: String,
    advisory_digest: String,
    supply_chain_digest: String,
    verifier_digest: String,
    run_id: String,
    protected_acceptance: bool,
    task_accepted: bool,
}
impl StaticEvidence {
    pub fn source_digest(&self) -> &str {
        &self.source_digest
    }
    pub fn digest(&self) -> Result<String> {
        Ok(digest(&serde_json::to_vec(self)?))
    }
}
impl QaReceipt {
    pub fn passed(&self) -> u64 {
        self.passed
    }
    pub fn total(&self) -> u64 {
        self.total
    }
    pub fn source_digest(&self) -> &str {
        &self.source_digest
    }
    pub fn package_digest(&self) -> &str {
        &self.package_digest
    }
    pub fn environment_digest(&self) -> &str {
        &self.environment_digest
    }
}
impl ProtectedQa {
    pub fn bundle(
        &self,
        mut program: BuiltProgram,
        source: &FrozenSource,
        supply: &crate::supply_chain::SupplyChainEvidence,
    ) -> Result<BuiltProgram> {
        self.authority()?;
        if source.snapshot().digest != program.source
            || supply.source_digest() != program.source
            || !supply.blockers().is_empty()
            || supply.notices().is_empty()
        {
            return Err(Error::Denied("distribution license/source binding"));
        }
        program
            .package
            .insert("LICENSE".into(), source.bytes("LICENSE")?.to_vec());
        program.package.insert(
            "THIRD_PARTY_NOTICES.txt".into(),
            supply.notices().as_bytes().to_vec(),
        );
        program.package.insert(
            "bom.cdx.json".into(),
            serde_json::to_vec_pretty(supply.document())?,
        );
        Ok(program)
    }
    pub fn new(config: QaConfig, store: LocalStore, profiles: ReviewProfiles) -> Result<Self> {
        config.limits.validate_quality()?;
        if [
            &config.task_id,
            &config.resource_id,
            &config.authorization_ref,
            &config.run_id,
        ]
        .iter()
        .any(|s| !bounded(s, 256))
        {
            return Err(Error::Invalid("QA host identities"));
        }
        let mut key = Zeroizing::new([0; 32]);
        getrandom::fill(&mut *key).map_err(|_| Error::Denied("QA entropy"))?;
        let mut host = Self {
            config,
            store,
            profiles,
            key,
            revoked: false,
            sequence: 0,
        };
        host.authority()?;
        let anchor = format!(
            "quality:protected-qa-session:{}",
            blake3::hash(host.config.task_id.as_bytes())
        );
        match host.store.get_artifact(&host.config.task_id, &anchor) {
            Ok(_) => {
                return Err(Error::Denied(
                    "QA task already has a session; inspect interrupted evidence before authorizing a new task",
                ));
            }
            Err(melissa_local_store::StoreError::NotFound(_)) => {}
            Err(error) => return Err(error.into()),
        }
        // Atomic create with random content gives concurrent constructors
        // different bytes: the authenticated artifact store admits only one.
        let payload = serde_json::to_vec(
            &json!({"schema_version":"melissa.qa-session/v1","run_id":host.config.run_id,"nonce":blake3::hash(&*host.key).to_hex().to_string(),"automatic_resume_or_retry":false}),
        )?;
        host.store.put_artifact(
            &ArtifactRequest {
                artifact_id: anchor,
                task_id: host.config.task_id.clone(),
                content_type: "application/json".into(),
                logical_type: "quality.protected.session".into(),
                schema_version: "v1".into(),
                retain_until_ms: 0,
                legal_hold: true,
            },
            &payload,
        )?;
        Ok(host)
    }
    pub fn revoke(&mut self) {
        self.revoked = true;
    }
    pub fn scan_static(
        &mut self,
        source: &FrozenSource,
        repo: &Repository,
        cancel: &AtomicBool,
    ) -> Result<StaticEvidence> {
        self.authority()?;
        source.current(repo, cancel)?;
        if self
            .store
            .journal()
            .task(&self.config.task_id)?
            .contract
            .input_snapshot_digest
            != source.snapshot().digest
        {
            return Err(Error::Stale("SAST task source binding"));
        }
        if !crate::security::scan_secrets(source)?.is_empty() {
            return Err(Error::Denied("source secret findings require review"));
        }
        let mut files = BTreeMap::new();
        for f in &source.snapshot().files {
            if f.path == "quality-rules.yml" {
                return Err(Error::Denied("reserved SAST rule path"));
            }
            files.insert(f.path.clone(), source.bytes(&f.path)?.to_vec());
        }
        files.insert(
            "quality-rules.yml".into(),
            crate::security::RULES.as_bytes().to_vec(),
        );
        let mut request = self.request(
            "/opt/melissa-sast/bin/semgrep",
            [
                "scan",
                "--metrics=off",
                "--disable-version-check",
                "--no-git-ignore",
                "--json",
                "--error",
                "--config",
                "/work/quality-rules.yml",
                "/work",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        );
        request.image_id = crate::security::SEMGREP_IMAGE.into();
        let (report, log) = self.execute(request, Snapshot { files }, cancel)?;
        if report.exit_code != Some(0) {
            let finding = serde_json::from_slice::<Value>(&report.stdout)
                .ok()
                .is_some_and(|v| {
                    v["results"].as_array().is_some_and(|a| !a.is_empty())
                        && v["errors"].as_array().is_some_and(Vec::is_empty)
                });
            if report.exit_code == Some(1) && finding {
                return Err(Error::Denied("native SAST findings"));
            }
            return Err(Error::Denied("native SAST reported failure or findings"));
        }
        let version = crate::security::semgrep_clean(&report.stdout)?;
        source.current(repo, cancel)?;
        let evidence = StaticEvidence {
            source_digest: source.snapshot().digest.clone(),
            rules_digest: crate::security::rules_digest(),
            image: crate::security::SEMGREP_IMAGE.into(),
            version,
            raw_log_digest: log,
        };
        self.record("static_receipt", &evidence)?;
        Ok(evidence)
    }
    pub fn verifier_digest() -> String {
        let mut hash = blake3::Hasher::new();
        hash.update(b"melissa.code11-protected-verifier/v1\0");
        for bytes in [
            include_bytes!("protected.rs").as_slice(),
            include_bytes!("cargo_driver.py"),
            include_bytes!("security.rs"),
            include_bytes!("semgrep-rules.yml"),
            include_bytes!("supply_chain.rs"),
            include_bytes!("verification_tools.rs"),
            include_bytes!("../../../tools/code11/upstream-license-pins.json"),
            include_bytes!("../../container-executor/src/lib.rs"),
            include_bytes!("../../container-executor/src/runtime.rs"),
        ] {
            hash.update(&(bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
        format!("blake3:{}", hash.finalize())
    }
    pub(crate) fn authority(&self) -> Result {
        let c = &self.config;
        let t = self.store.journal().task(&c.task_id)?;
        let role = "melissa.core.independent-qa";
        let p = self
            .profiles
            .profile(role)
            .ok_or(Error::Denied("QA profile absent"))?;
        if self.revoked
            || t.record.state != melissa_core_contracts::TaskState::Running
            || t.record.unknown_invocation_id.is_some()
            || t.contract.workspace_ref != c.resource_id
            || !t.contract.allowed_resource_ids.contains(&c.resource_id)
            || !t.contract.authorization_refs.contains(&c.authorization_ref)
            || !t.contract.selected_profile_ids.iter().any(|s| s == role)
            || melissa_execution_broker::unix_ms().map_err(|_| Error::Denied("QA clock"))?
                >= c.expires_at_ms
            || !p.tool_ids.iter().any(|s| s == "verify.run")
        {
            return Err(Error::Denied("QA task/role/resource/expiry"));
        }
        for effect in [
            "workspace.read",
            "verification.run_protected",
            "evidence.append",
        ] {
            if !t.contract.allowed_effects.iter().any(|s| s == effect)
                || !p.authority.capability_ceiling.iter().any(|s| s == effect)
                || p.authority.forbidden_effects.iter().any(|s| s == effect)
            {
                return Err(Error::Denied("QA effect intersection"));
            }
        }
        let l = &c.limits;
        if l.wall_time_ms > t.contract.limits.max_wall_time_ms
            || l.disk_bytes > t.contract.limits.max_disk_bytes
            || l.output_bytes + l.export_bytes > t.contract.limits.max_output_bytes
        {
            return Err(Error::Denied("QA task resource envelope"));
        }
        Ok(())
    }
    pub(crate) fn tool_identity(&self) -> (&str, &str) {
        (&self.config.task_id, &self.config.run_id)
    }
    pub fn evidence_connection(&self) -> Result<LocalStore> {
        Ok(self.store.connect()?)
    }
    fn record(&mut self, kind: &str, value: &impl Serialize) -> Result<String> {
        self.sequence += 1;
        let bytes = serde_json::to_vec(value)?;
        let id = format!("qa:{}:{}:{kind}", self.config.run_id, self.sequence);
        self.store
            .put_artifact(
                &ArtifactRequest {
                    artifact_id: id,
                    task_id: self.config.task_id.clone(),
                    content_type: "application/json".into(),
                    logical_type: format!("quality.protected.{kind}"),
                    schema_version: "v1".into(),
                    retain_until_ms: 0,
                    legal_hold: true,
                },
                &bytes,
            )
            .map_err(|_| Error::AfterAppend)?;
        Ok(digest(&bytes))
    }
    fn execute(
        &mut self,
        mut request: ContainerRequest,
        snapshot: Snapshot,
        cancel: &AtomicBool,
    ) -> Result<(ContainerReport, String)> {
        self.authority()?;
        request.limits.wall_time_ms =
            request
                .limits
                .wall_time_ms
                .min(self.config.expires_at_ms.saturating_sub(
                    melissa_execution_broker::unix_ms().map_err(|_| Error::Denied("QA clock"))?,
                ));
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        // Durable intent precedes every native workload. A failed or uncertain
        // executor has no automatic retry or protected receipt.
        self.record("intent",&json!({"request":request,"input_digest":snapshot.digest().map_err(|_|Error::Invalid("QA input"))?,"verifier_digest":Self::verifier_digest()}))?;
        let mut owned_container = None;
        let result = Executor::run_with_observer(&request, &snapshot, cancel, &mut |id| {
            owned_container = Some(id.to_owned());
            self.record(
                "owned_container",
                &json!({"container_id":id,"run_id":self.config.run_id,"automatic_retry":false}),
            )
            .map(|_| ())
            .map_err(|_| {
                melissa_container_executor::ExecutorError::Control(
                    "QA ownership evidence unavailable".into(),
                )
            })
        });
        let report = match result {
            Ok(r) => r,
            Err(melissa_container_executor::ExecutorError::CancelledBeforeLaunch) => {
                // This exact CODE03 variant is returned before creation or
                // only after confirmed prelaunch cleanup. Preserve that typed
                // truth; generic control/IO errors remain unconfirmed below.
                self.record("cancelled_before_launch",&json!({"container_id":owned_container,"image_id":request.image_id,"execution_contract":melissa_container_executor::EXECUTION_CONTRACT,"workload_started":false,"cleanup_confirmed":true,"tree_confirmed_stopped":true,"task_accepted":false}))?;
                return Err(Error::Cancelled);
            }
            Err(error) => {
                self.record(
                    "execution_error",
                    &json!({"status":"BLOCKED","reason":error.to_string(),"retry_automatically":false}),
                )?;
                self.revoked = true;
                return Err(Error::Denied(
                    "isolated QA executor unavailable; no host fallback",
                ));
            }
        };
        let log = self.record("raw_execution", &report)?;
        self.authority()?;
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        if report.image_id != request.image_id
            || report.input_digest != snapshot.digest().map_err(|_| Error::Invalid("QA input"))?
            || !report.policy_verified
            || !report.cleanup_confirmed
            || !report.tree_confirmed_stopped
            || !report.disk_limit_hard_enforced
            || report.output_truncated
            || report.export_truncated
            || !report.orphan_process_ids.is_empty()
            || report.monitor_error.is_some()
            || report.termination != TerminationReason::Exited
        {
            self.revoked = true;
            return Err(Error::Denied("QA execution incomplete or uncertain"));
        }
        Ok((report, log))
    }
    fn request(&self, executable: &str, argv: Vec<String>) -> ContainerRequest {
        ContainerRequest {
            schema_version: "melissa.container-exec/v1".into(),
            image_id: SDK_IMAGE.into(),
            executable: executable.into(),
            argv,
            cwd: ".".into(),
            env: BTreeMap::new(),
            limits: self.config.limits.clone(),
        }
    }
    pub fn build(
        &mut self,
        source: &FrozenSource,
        repo: &Repository,
        spec: &ProgramSpec,
        cancel: &AtomicBool,
    ) -> Result<BuiltProgram> {
        self.authority()?;
        source.current(repo, cancel)?;
        if self
            .store
            .journal()
            .task(&self.config.task_id)?
            .contract
            .input_snapshot_digest
            != source.snapshot().digest
        {
            return Err(Error::Stale("QA task source binding"));
        }
        if spec.language == Language::RustCargo {
            return self.build_cargo(source, repo, &spec.entry, cancel);
        }
        let bytes = source.bytes(&spec.entry)?.to_vec();
        if bytes.is_empty() {
            return Err(Error::Invalid("empty program"));
        }
        let (filename, executable, arguments) = match spec.language {
            Language::Rust | Language::RustCargo => ("program.rs", "/usr/bin/python3", vec!["-I".into(), "-S".into(), "-c".into(), "import os,sys;os.chmod('/work/program',0o500);os.execv('/work/program',['program',*sys.argv[1:]])".into()]),
            Language::Python => (
                "program.py",
                "/opt/melissa-python/bin/python",
                vec!["-I".into(), "-S".into(), "/work/program.py".into()],
            ),
            Language::JavaScript => (
                "program.mjs",
                "/opt/node/bin/node",
                vec!["/work/program.mjs".into()],
            ),
        };
        let mut package = BTreeMap::from([(filename.into(), bytes)]);
        let build_log = if spec.language == Language::Rust {
            let request = self.request(
                "/usr/local/cargo/bin/rustc",
                vec![
                    filename.into(),
                    "--edition=2021".into(),
                    "-O".into(),
                    "-o".into(),
                    "program".into(),
                ],
            );
            let (report, log) = self.execute(
                request,
                Snapshot {
                    files: package.clone(),
                },
                cancel,
            )?;
            if report.exit_code != Some(0) {
                return Err(Error::Invalid("native build failed"));
            }
            let files = inspect_ustar(
                &report.archive,
                &Limits {
                    max_file_bytes: 32 * 1024 * 1024,
                    max_total_bytes: 48 * 1024 * 1024,
                    ..Limits::default()
                },
            )?;
            if files.len() != 2 {
                return Err(Error::Denied("build export contains unexpected files"));
            }
            let files: BTreeMap<_, _> = files.into_iter().map(|f| (f.path, f.bytes)).collect();
            if files.get(filename) != package.get(filename)
                || !files
                    .get("program")
                    .is_some_and(|b| b.starts_with(b"\x7fELF"))
            {
                return Err(Error::Denied("build output/source identity"));
            }
            package = files;
            log
        } else {
            self.record("interpreted_package",&json!({"source_digest":source.snapshot().digest,"language":spec.language,"entry_digest":digest(package.get(filename).unwrap())}))?
        };
        source.current(repo, cancel)?;
        Ok(BuiltProgram {
            source: source.snapshot().digest.clone(),
            environment: digest(SDK_IMAGE.as_bytes()),
            package,
            executable: executable.into(),
            arguments,
            build_log,
        })
    }
    /// Opt-in project package for CLI v2. Legacy single-file build semantics
    /// remain unchanged. Every visible source/config/consumer file is pinned
    /// and included; private QA inputs are never added to this package.
    pub fn build_project(
        &mut self,
        source: &FrozenSource,
        repo: &Repository,
        spec: &ProgramSpec,
        cancel: &AtomicBool,
    ) -> Result<BuiltProgram> {
        self.authority()?;
        source.current(repo, cancel)?;
        if self
            .store
            .journal()
            .task(&self.config.task_id)?
            .contract
            .input_snapshot_digest
            != source.snapshot().digest
        {
            return Err(Error::Stale("QA project source binding"));
        }
        if !source.snapshot().files.iter().any(|f| f.path == spec.entry)
            || source.bytes(&spec.entry)?.is_empty()
        {
            return Err(Error::Invalid("project entry"));
        }
        let mut package = source
            .snapshot()
            .files
            .iter()
            .map(|f| Ok((f.path.clone(), source.bytes(&f.path)?.to_vec())))
            .collect::<Result<BTreeMap<_, _>>>()?;
        Snapshot {
            files: package.clone(),
        }
        .validate(self.config.limits.disk_bytes)
        .map_err(|_| Error::Invalid("project snapshot envelope"))?;
        if spec.language == Language::RustCargo {
            let mut built = self.build_cargo(source, repo, &spec.entry, cancel)?;
            if package.contains_key("program") {
                return Err(Error::Denied("project collides with compiled program"));
            }
            package.extend(built.package);
            built.package = package;
            return Ok(built);
        }
        let (executable,arguments,build_log)=match spec.language {
            Language::Python=> ("/opt/melissa-python/bin/python",vec!["-I".into(),"-S".into(),"-B".into(),"-c".into(),"import runpy,sys;entry=sys.argv[1];sys.argv=sys.argv[1:];sys.path.insert(0,'/work');runpy.run_path(entry,run_name='__main__')".into(),format!("/work/{}",spec.entry)],self.record("interpreted_project",&json!({"source_digest":source.snapshot().digest,"language":spec.language,"entry":spec.entry,"files":package.len()}))?),
            Language::JavaScript=>("/opt/node/bin/node",vec![format!("/work/{}",spec.entry)],self.record("interpreted_project",&json!({"source_digest":source.snapshot().digest,"language":spec.language,"entry":spec.entry,"files":package.len()}))?),
            Language::Rust=>{
                if package.contains_key("program"){return Err(Error::Denied("project collides with compiled program"));}
                let request=self.request("/usr/local/cargo/bin/rustc",vec!["--edition=2021".into(),"-O".into(),"-o".into(),"program".into(),"--".into(),spec.entry.clone()]);
                let (report,log)=self.execute(request,Snapshot{files:package.clone()},cancel)?;
                if report.exit_code!=Some(0){return Err(Error::Invalid("native project build failed"));}
                let files: BTreeMap<_,_>=inspect_ustar(&report.archive,&Limits{max_file_bytes:32*1024*1024,max_total_bytes:48*1024*1024,..Limits::default()})?.into_iter().map(|f|(f.path,f.bytes)).collect();
                if files.len()!=package.len()+1||package.iter().any(|(p,b)|files.get(p)!=Some(b))||!files.get("program").is_some_and(|b|b.starts_with(b"\x7fELF")){return Err(Error::Denied("project build source/output identity"));}
                package=files;
                ("/usr/bin/python3",vec!["-I".into(),"-S".into(),"-c".into(),"import os,sys;os.chmod('/work/program',0o500);os.execv('/work/program',['program',*sys.argv[1:]])".into()],log)
            },
            Language::RustCargo=>unreachable!(),
        };
        source.current(repo, cancel)?;
        Ok(BuiltProgram {
            source: source.snapshot().digest.clone(),
            environment: digest(SDK_IMAGE.as_bytes()),
            package,
            executable: executable.into(),
            arguments,
            build_log,
        })
    }
    fn build_cargo(
        &mut self,
        source: &FrozenSource,
        repo: &Repository,
        binary: &str,
        cancel: &AtomicBool,
    ) -> Result<BuiltProgram> {
        if binary.is_empty()
            || binary.len() > 100
            || !binary
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(Error::Invalid("Cargo binary identity"));
        }
        source.bytes("Cargo.toml")?;
        source.bytes("Cargo.lock")?;
        let mut inputs = BTreeMap::new();
        for f in &source.snapshot().files {
            if f.path == "program"
                || f.path.split('/').any(|c| {
                    matches!(
                        c,
                        "qa-build"
                            | ".cargo"
                            | ".melissa-runtime"
                            | "rust-toolchain"
                            | "rust-toolchain.toml"
                    )
                })
            {
                return Err(Error::Denied(
                    "Cargo reserved path or toolchain/config override",
                ));
            }
            inputs.insert(f.path.clone(), source.bytes(&f.path)?.to_vec());
        }
        let request = self.request(
            "/usr/bin/python3",
            vec![
                "-I".into(),
                "-S".into(),
                "-c".into(),
                include_str!("cargo_driver.py").into(),
                binary.into(),
            ],
        );
        let (report, build_log) = self.execute(
            request,
            Snapshot {
                files: inputs.clone(),
            },
            cancel,
        )?;
        if report.exit_code != Some(0) {
            return Err(Error::Invalid("isolated locked/offline Cargo build failed"));
        }
        let exported = inspect_ustar(
            &report.archive,
            &Limits {
                max_file_bytes: 32 * 1024 * 1024,
                max_total_bytes: 48 * 1024 * 1024,
                ..Limits::default()
            },
        )?;
        let exported: BTreeMap<_, _> = exported.into_iter().map(|f| (f.path, f.bytes)).collect();
        if inputs.iter().any(|(p, b)| exported.get(p) != Some(b)) {
            return Err(Error::Stale("Cargo build changed captured source inputs"));
        }
        let program = exported
            .get("program")
            .filter(|b| b.starts_with(b"\x7fELF"))
            .ok_or(Error::Invalid("Cargo executable missing"))?
            .clone();
        source.current(repo, cancel)?;
        Ok(BuiltProgram{source:source.snapshot().digest.clone(),environment:digest(SDK_IMAGE.as_bytes()),package:BTreeMap::from([("program".into(),program)]),executable:"/usr/bin/python3".into(),arguments:vec!["-I".into(),"-S".into(),"-c".into(),"import os,sys;os.chmod('/work/program',0o500);os.execv('/work/program',['program',*sys.argv[1:]])".into()],build_log})
    }
    fn evaluate(
        &mut self,
        program: &BuiltProgram,
        suite: &Suite,
        cancel: &AtomicBool,
    ) -> Result<(u64, Vec<String>)> {
        let mut passed = 0;
        let mut logs = vec![program.build_log.clone()];
        for c in &suite.cases {
            let mut args = program.arguments.clone();
            args.push(c.input.to_string());
            let (report, log) = self.execute(
                self.request(&program.executable, args),
                Snapshot {
                    files: program.package.clone(),
                },
                cancel,
            )?;
            // A fresh container per case prevents one case corrupting the next.
            // Expected results and remaining cases are never in the snapshot.
            let response = std::str::from_utf8(&report.stdout)
                .ok()
                .and_then(|s| melissa_model_gateway::strict_json(s.trim()).ok());
            if report.exit_code == Some(0) && response.as_ref() == Some(&c.expected) {
                passed += 1;
            }
            logs.push(log);
        }
        Ok((passed, logs))
    }
    /// Positive and deliberately faulty controls must use the same protected
    /// suite. This function cannot accept deserialized developer test claims.
    pub fn verify(
        &mut self,
        source: &FrozenSource,
        repo: &Repository,
        program: &BuiltProgram,
        negative: &BuiltProgram,
        suite: &Suite,
        cancel: &AtomicBool,
    ) -> Result<QaReceipt> {
        self.authority()?;
        source.current(repo, cancel)?;
        if self
            .store
            .journal()
            .task(&self.config.task_id)?
            .contract
            .acceptance_contract_digest
            != suite.digest
        {
            return Err(Error::Denied(
                "protected suite differs from task acceptance contract",
            ));
        }
        if program.source != source.snapshot().digest
            || program.package_digest()? == negative.package_digest()?
            || program.environment != negative.environment
        {
            return Err(Error::Invalid("QA source or negative control binding"));
        }
        let (negative_passed, negative_logs) = self.evaluate(negative, suite, cancel)?;
        let (passed, logs) = self.evaluate(program, suite, cancel)?;
        let total = suite.cases.len() as u64;
        let log_digest=self.record("suite_result",&json!({"source_digest":program.source,"suite_digest":suite.digest,"passed":passed,"total":total,"negative_passed":negative_passed,"positive_logs":logs,"negative_logs":negative_logs}))?;
        source.current(repo, cancel)?;
        self.authority()?;
        if passed != total || negative_passed == total {
            return Err(Error::Denied(
                "protected QA positive or negative control failed",
            ));
        }
        let mut receipt = QaReceipt {
            source_digest: program.source.clone(),
            package_digest: program.package_digest()?,
            environment_digest: program.environment.clone(),
            suite_digest: suite.digest.clone(),
            verifier_digest: Self::verifier_digest(),
            log_digest,
            passed,
            total,
            negative_control_rejected: true,
            run_id: self.config.run_id.clone(),
            task_id: self.config.task_id.clone(),
            mac: String::new(),
        };
        receipt.mac = self.mac(&receipt)?;
        self.record("receipt", &receipt)?;
        Ok(receipt)
    }
    fn mac(&self, r: &QaReceipt) -> Result<String> {
        let payload = json!([
            "melissa.protected-qa/v1",
            r.source_digest,
            r.package_digest,
            r.environment_digest,
            r.suite_digest,
            r.verifier_digest,
            r.log_digest,
            r.passed,
            r.total,
            r.negative_control_rejected,
            r.run_id,
            r.task_id
        ]);
        Ok(
            blake3::keyed_hash(&self.key, &serde_json::to_vec(&payload)?)
                .to_hex()
                .to_string(),
        )
    }
    pub fn validate_receipt(&self, r: &QaReceipt) -> Result {
        self.authority()?;
        if r.task_id != self.config.task_id
            || r.run_id != self.config.run_id
            || r.mac != self.mac(r)?
            || r.total < 2
            || r.passed != r.total
            || !r.negative_control_rejected
            || r.verifier_digest != Self::verifier_digest()
            || r.source_digest
                != self
                    .store
                    .journal()
                    .task(&self.config.task_id)?
                    .contract
                    .input_snapshot_digest
        {
            return Err(Error::Denied("protected QA receipt authentication"));
        }
        Ok(())
    }
}
impl ProtectedQa {
    /// Only native adapter evidence can construct final acceptance; model claims
    /// cannot be deserialized into any of these opaque evidence types.
    #[allow(clippy::too_many_arguments)]
    pub fn accept(
        &mut self,
        source: &FrozenSource,
        repo: &Repository,
        package: &Repository,
        receipt: &QaReceipt,
        static_scan: &StaticEvidence,
        audit: &crate::security::AuditEvidence,
        supply: &crate::supply_chain::SupplyChainEvidence,
        cancel: &AtomicBool,
    ) -> Result<ReleaseAcceptance> {
        self.validate_receipt(receipt)?;
        source.current(repo, cancel)?;
        audit.current()?;
        if [
            receipt.source_digest(),
            static_scan.source_digest(),
            audit.source_digest(),
            supply.source_digest(),
        ]
        .iter()
        .any(|s| *s != source.snapshot().digest)
            || !supply.blockers().is_empty()
            || supply.notices().is_empty()
        {
            return Err(Error::Denied("release evidence source or license gate"));
        }
        let captured = FrozenSource::capture(package, cancel)?;
        if !captured.snapshot().excluded_prefixes.is_empty() {
            return Err(Error::Denied("release package has exclusions"));
        }
        let files: BTreeMap<String, Vec<u8>> = captured
            .snapshot()
            .files
            .iter()
            .map(|f| Ok((f.path.clone(), captured.bytes(&f.path)?.to_vec())))
            .collect::<Result<_>>()?;
        if files.get("THIRD_PARTY_NOTICES.txt").map(Vec::as_slice)
            != Some(supply.notices().as_bytes())
            || files.get("LICENSE").map(Vec::as_slice) != Some(source.bytes("LICENSE")?)
            || files.get("bom.cdx.json") != Some(&serde_json::to_vec_pretty(supply.document())?)
        {
            return Err(Error::Denied("distribution notices or SBOM differ"));
        }
        if digest(&serde_json::to_vec(&files)?) != receipt.package_digest {
            return Err(Error::Stale("smoke-tested package bytes differ"));
        }
        source.current(repo, cancel)?;
        self.authority()?;
        let acceptance = ReleaseAcceptance {
            schema_version: "melissa.protected-release-acceptance/v1",
            source_digest: source.snapshot().digest.clone(),
            package_digest: receipt.package_digest.clone(),
            qa_receipt_digest: digest(&serde_json::to_vec(receipt)?),
            security_digest: static_scan.digest()?,
            advisory_digest: audit.digest()?,
            supply_chain_digest: supply.digest()?,
            verifier_digest: Self::verifier_digest(),
            run_id: self.config.run_id.clone(),
            protected_acceptance: true,
            task_accepted: true,
        };
        self.record("release_acceptance", &acceptance)?;
        Ok(acceptance)
    }
}
trait QualityLimits {
    fn validate_quality(&self) -> Result;
}
impl QualityLimits for ContainerLimits {
    fn validate_quality(&self) -> Result {
        ContainerRequest {
            schema_version: "melissa.container-exec/v1".into(),
            image_id: SDK_IMAGE.into(),
            executable: "/usr/bin/true".into(),
            argv: vec![],
            cwd: ".".into(),
            env: BTreeMap::new(),
            limits: self.clone(),
        }
        .validate()
        .map_err(|_| Error::Invalid("QA resource envelope"))
    }
}
#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::{fs, path::Path};
    fn host(dir: &Path, effect: bool) -> Result<ProtectedQa> {
        let path = dir.join("source");
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("main.rs"), "fn main(){}").unwrap();
        let repo = Repository::open(&path, Limits::default(), vec![]).unwrap();
        let source = FrozenSource::capture(&repo, &AtomicBool::new(false)).unwrap();
        let mut task: melissa_core_contracts::wire::TaskContractV1 = serde_json::from_slice(
            include_bytes!("../../../tests/fixtures/code01/wire/task-contract-v1.json"),
        )
        .unwrap();
        task.task_id = "task:protected-unit".into();
        task.workspace_ref = "workspace:unit".into();
        task.selected_profile_ids = vec!["melissa.core.independent-qa".into()];
        task.allowed_resource_ids = vec![task.workspace_ref.clone()];
        task.authorization_refs = vec!["auth:unit".into()];
        task.allowed_effects = vec!["workspace.read".into(), "evidence.append".into()];
        if effect {
            task.allowed_effects
                .push("verification.run_protected".into());
        }
        task.input_snapshot_digest = source.snapshot().digest.clone();
        task.limits.max_disk_bytes = 512 * 1024 * 1024;
        task.limits.max_output_bytes = 64 * 1024 * 1024;
        task.limits.max_wall_time_ms = 90000;
        let mut store = LocalStore::open(
            &dir.join("store"),
            melissa_local_store::StoreKey::from_bytes([19; 32]),
            melissa_local_store::StoreConfig::default(),
        )
        .unwrap();
        store.journal_mut().create_task(&task).unwrap();
        for state in [
            melissa_core_contracts::TaskState::Planned,
            melissa_core_contracts::TaskState::Queued,
            melissa_core_contracts::TaskState::Leased,
            melissa_core_contracts::TaskState::Running,
        ] {
            let v = store.journal().task(&task.task_id).unwrap().record.version;
            store
                .transition_task(&task.task_id, v, state, None)
                .unwrap();
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let profiles = ReviewProfiles::load_from_repository(
            &Repository::open(root, Limits::default(), vec![]).unwrap(),
        )
        .unwrap();
        let config = QaConfig {
            task_id: task.task_id,
            resource_id: task.workspace_ref,
            authorization_ref: "auth:unit".into(),
            run_id: "unit".into(),
            expires_at_ms: melissa_execution_broker::unix_ms().unwrap() + 60000,
            limits: ContainerLimits {
                wall_time_ms: 30000,
                cpu_time_ms: 20000,
                cpu_rate_basis_points: 10000,
                memory_bytes: 1024 * 1024 * 1024,
                disk_bytes: 512 * 1024 * 1024,
                output_bytes: 4 * 1024 * 1024,
                export_bytes: 16 * 1024 * 1024,
                max_processes: 128,
            },
        };
        ProtectedQa::new(config, store, profiles)
    }
    #[test]
    fn zero_and_duplicate_protected_cases_are_rejected() {
        assert!(Suite::new(vec![]).is_err());
        assert!(
            Suite::new(vec![Case {
                id: "one".into(),
                input: json!(1),
                expected: json!(1)
            }])
            .is_err()
        );
        assert!(
            Suite::new(vec![
                Case {
                    id: "a".into(),
                    input: json!(1),
                    expected: json!(1)
                },
                Case {
                    id: "b".into(),
                    input: json!(1),
                    expected: json!(2)
                }
            ])
            .is_err()
        );
    }
    #[test]
    fn protected_receipt_cannot_cross_hosts_be_tampered_or_survive_revocation() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let mut first = host(a.path(), true).unwrap();
        let second = host(b.path(), true).unwrap();
        let d = first
            .store
            .journal()
            .task("task:protected-unit")
            .unwrap()
            .contract
            .input_snapshot_digest;
        let mut r = QaReceipt {
            source_digest: d.clone(),
            package_digest: d.clone(),
            environment_digest: d.clone(),
            suite_digest: d.clone(),
            verifier_digest: ProtectedQa::verifier_digest(),
            log_digest: d,
            passed: 2,
            total: 2,
            negative_control_rejected: true,
            run_id: "unit".into(),
            task_id: "task:protected-unit".into(),
            mac: String::new(),
        };
        r.mac = first.mac(&r).unwrap();
        assert!(first.validate_receipt(&r).is_ok());
        assert!(second.validate_receipt(&r).is_err());
        r.total = 3;
        assert!(first.validate_receipt(&r).is_err());
        r.total = 2;
        first.revoke();
        assert!(first.validate_receipt(&r).is_err());
    }
    #[test]
    fn removed_task_permission_and_cancellation_cannot_start_work() {
        let a = tempfile::tempdir().unwrap();
        let mut qa = host(a.path(), true).unwrap();
        let repo = Repository::open(&a.path().join("source"), Limits::default(), vec![]).unwrap();
        let s = FrozenSource::capture(&repo, &AtomicBool::new(false)).unwrap();
        assert!(matches!(
            qa.build(
                &s,
                &repo,
                &ProgramSpec {
                    language: Language::Rust,
                    entry: "main.rs".into()
                },
                &AtomicBool::new(true)
            ),
            Err(Error::Cancelled) | Err(Error::Repository(_))
        ));
        qa.revoke();
        assert!(qa.authority().is_err());
        let denied = tempfile::tempdir().unwrap();
        assert!(host(denied.path(), false).is_err());
    }
    #[test]
    fn interrupted_or_finished_task_cannot_open_a_new_qa_session_silently() {
        let dir = tempfile::tempdir().unwrap();
        let first = host(dir.path(), true).unwrap();
        let c = &first.config;
        let config = QaConfig {
            task_id: c.task_id.clone(),
            resource_id: c.resource_id.clone(),
            authorization_ref: c.authorization_ref.clone(),
            run_id: "new-run-cannot-bypass-existing-task".into(),
            expires_at_ms: c.expires_at_ms,
            limits: c.limits.clone(),
        };
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let profiles = ReviewProfiles::load_from_repository(
            &Repository::open(root, Limits::default(), vec![]).unwrap(),
        )
        .unwrap();
        assert!(ProtectedQa::new(config, first.store.connect().unwrap(), profiles).is_err());
    }
}
