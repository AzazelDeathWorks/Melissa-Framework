//! Source-aware, offline goal preparation and authority-preserving plan revision.
use crate::{
    Error, Result,
    app::{App, absolute, bytes},
    digest,
    recipe::{Recipe, Step},
};
use melissa_repository::{Limits, Repository};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::Write,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};
fn intent(goal: &str) -> (&'static str, bool) {
    let s = goal.to_lowercase();
    if ["review", "incele", "denetle"]
        .iter()
        .any(|w| s.contains(w))
    {
        ("reviewer", false)
    } else if ["bug", "hata", "düzelt", "fix", "debug"]
        .iter()
        .any(|w| s.contains(w))
    {
        ("debugger", true)
    } else if ["migration", "veritaban", "database", "sql"]
        .iter()
        .any(|w| s.contains(w))
    {
        ("database", true)
    } else if ["frontend", "arayüz", "responsive", "css"]
        .iter()
        .any(|w| s.contains(w))
    {
        ("frontend", true)
    } else if ["backend", "endpoint", "api"].iter().any(|w| s.contains(w)) {
        ("backend", true)
    } else if ["bağımlılık", "upgrade", "dependency"]
        .iter()
        .any(|w| s.contains(w))
    {
        ("dependency-build", true)
    } else if ["test", "regresyon"].iter().any(|w| s.contains(w)) {
        ("test-engineer", true)
    } else {
        ("implementer", true)
    }
}
impl App {
    pub fn prepare(
        &self,
        workspace: &Path,
        goal: &str,
        out: &Path,
        acceptance: Option<&Path>,
    ) -> Result<Value> {
        self.prepare_mode(
            workspace,
            goal,
            out,
            acceptance,
            crate::conversation::Mode::Develop,
        )
    }
    pub fn prepare_mode(
        &self,
        workspace: &Path,
        goal: &str,
        out: &Path,
        acceptance: Option<&Path>,
        mode: crate::conversation::Mode,
    ) -> Result<Value> {
        if goal.trim().is_empty() || goal.len() > 16384 {
            return Err(Error::Usage("Görev amacı geçersiz."));
        }
        let root = absolute(workspace)?;
        let out = absolute(out)?;
        let inside = |p: &Path| {
            let candidate = p.to_string_lossy().replace('/', "\\").to_uppercase();
            let base = root.to_string_lossy().replace('/', "\\").to_uppercase();
            candidate == base
                || candidate.starts_with(&(base.trim_end_matches('\\').to_owned() + "\\"))
        };
        if inside(&out) || acceptance.is_some_and(|p| absolute(p).is_ok_and(|p| inside(&p))) {
            return Err(Error::Denied(
                "Özel görev tarifi kaynak projesi dışında olmalı.",
            ));
        }
        let repo = Repository::open(&root, Limits::default(), vec![])?;
        self.require_separate_workspace(&repo)?;
        let snap = repo.snapshot(&AtomicBool::new(false))?;
        let acceptance_template = acceptance.map(Recipe::read).transpose()?;
        let (role, write) = intent(goal);
        let (role, write) = match mode {
            crate::conversation::Mode::Plan => ("repository-analyst", false),
            crate::conversation::Mode::Review => ("reviewer", false),
            crate::conversation::Mode::Develop => (role, write),
        };
        let mentioned = snap
            .files
            .iter()
            .filter(|f| goal.contains(&f.path))
            .map(|f| f.path.clone())
            .collect::<BTreeSet<_>>();
        let tests = snap
            .files
            .iter()
            .filter(|f| {
                f.path.contains("test") || f.path.contains("spec") || f.path.ends_with("AGENTS.md")
            })
            .map(|f| f.path.clone())
            .collect::<BTreeSet<_>>();
        let candidates = snap
            .files
            .iter()
            .filter(|f| {
                !tests.contains(&f.path)
                    && melissa_repository::discovery::language(&f.path).is_some()
            })
            .map(|f| f.path.clone())
            .collect::<BTreeSet<_>>();
        let writable = if write {
            if mentioned.is_empty() {
                candidates.clone()
            } else {
                mentioned.difference(&tests).cloned().collect()
            }
        } else {
            BTreeSet::new()
        };

        let affected = if !mentioned.is_empty() {
            mentioned.clone()
        } else {
            candidates.clone()
        };
        let dependency_evidence = dependency_hints(&repo, &snap, &affected)?;
        let lower = goal.to_lowercase();
        let signals = melissa_workflow::adaptive::Signals {
            repository_files: snap.files.len(),
            affected_files: affected.len(),
            affected_modules: affected
                .iter()
                .filter_map(|p| p.rsplit_once('/').map(|(module, _)| module))
                .collect::<BTreeSet<_>>()
                .len(),
            cross_module_dependencies: dependency_evidence["cross_file_edges"]
                .as_u64()
                .unwrap_or(0) as usize,
            test_files: tests.len(),
            high_risk: [
                "security",
                "auth",
                "payment",
                "migration",
                "güvenlik",
                "kimlik",
            ]
            .iter()
            .any(|s| lower.contains(s)),
            architecture_change: ["architecture", "mimari", "refactor"]
                .iter()
                .any(|s| lower.contains(s)),
            compiler_failure: ["compile", "compiler", "derleme", "build failed"]
                .iter()
                .any(|s| lower.contains(s)),
            estimated_cost_micro_usd: None,
            authorized_cost_micro_usd: acceptance_template.as_ref().map(|r| r.max_cost_micro_usd),
        };
        let catalog = crate::resources::catalog()?;
        let profiles = melissa_workflow::adaptive::specialists(&signals, role, &catalog)
            .map_err(|_| Error::Denied("Göreve uygun çalıştırılabilir uzman bulunamadı."))?;
        let last_test = profiles
            .iter()
            .rposition(|p| p == "melissa.code.test-engineer");
        let agents = profiles.into_iter().enumerate().map(|(index,profile)| {
            let can_write = write && !writable.is_empty()
                && !matches!(profile.as_str(), "melissa.code.repository-analyst" | "melissa.code.reviewer" | "melissa.code.security-reviewer" | "melissa.code.architect")
                && Some(index) != last_test;
            let purpose = match profile.as_str() {
                "melissa.code.repository-analyst" => "Identify affected source, dependencies and evidence",
                "melissa.code.architect" => "Design a scoped change and explicit verification milestones",
                "melissa.code.test-engineer" if can_write => "Design and implement tests within the approved writable scope, retaining baseline failure evidence",
                "melissa.code.test-engineer" => "Execute authorized tests and retain diagnostics with source versions",
                "melissa.code.reviewer" => "Independently review the final source, changes and recorded tests",
                "melissa.code.security-reviewer" => "Review authorization boundaries and security consequences",
                _ => "Diagnose and implement the authorized change, retaining failed attempts and evidence",
            };
            Step { profile, goal: format!("{purpose}. Task goal: {goal}"), write: can_write }
        }).collect::<Vec<_>>();
        let adaptive = json!({
            "mode":mode,"write_intent":write,"source_digest":snap.digest,"guidance":crate::instructions::load(&self.home,&repo,&AtomicBool::new(false))?,
            "task_goal":goal, "complexity":melissa_workflow::adaptive::complexity(&signals),
            "signals":signals, "specialist_selection":melissa_workflow::adaptive::selection_explanation(&signals,&agents.iter().map(|s|s.profile.clone()).collect::<Vec<_>>()), "dependency_evidence":dependency_evidence,
            "scope":{"inspected_paths":affected,"proposed_writes":writable},
            "acceptance_criteria":if acceptance_template.is_some(){"operator acceptance template; private expected values withheld"}else{"reviewed project requirements and source-bound checks; execution/environment contract in Phase 3; no private oracle invented"},
            "risk":{"high_risk":signals.high_risk,"authority_expansion_allowed":false},
            "authorized_budget":acceptance_template.as_ref().map(|r|json!({"max_cost_micro_usd":r.max_cost_micro_usd,"wall_time_ms":r.wall_time_ms})), "constraints":["original task authority","one active specialist","operator-owned acceptance"],
            "milestones":agents.iter().enumerate().map(|(i,s)| json!({"id":format!("step-{i}"),"goal":s.goal,"profile":s.profile,"write":s.write,"dependencies":if i==0{vec![]}else{vec![format!("step-{}",i-1)]}})).collect::<Vec<_>>(), "required_specialists":agents.iter().map(|s| &s.profile).collect::<Vec<_>>(),
            "verification_strategy":if acceptance_template.is_some(){"specialist tests, independent final review, then protected host QA"}else{"discover project checks and retain baseline, implementation and final review evidence; not executed in preparation"},
            "budget_estimate":"unknown until provider configuration and measured usage are available"
        });
        let mut ready = false;
        let recipe = if let Some(mut r) = acceptance_template {
            // The operator's acceptance template owns writes, oracle, runtime
            // and cost. Goal inference is never allowed to broaden that scope.
            if write && !writable.is_subset(&r.writable_paths) {
                return Err(Error::Denied(
                    "Önerilen dosyalar kabul şablonunun yazma kapsamını aşıyor.",
                ));
            }
            r.agents = agents.clone();
            r.validate()?;
            ready = true;
            serde_json::to_value(r).map_err(|_| Error::Store)?
        } else {
            json!({"agents":agents,"writable_paths":writable,"protected_paths":tests,"language_hint":crate::development::detected_language(&repo)?.map(|l|l.wire()),"development":"project environment/execution contract in Phase 3","required_operator_inputs":["reviewed project requirements and checks","task duration and cost authorization before execution"],"private_oracle_required":false})
        };
        if repo.snapshot(&AtomicBool::new(false))?.digest != snap.digest {
            return Err(Error::Conflict("Source changed during task preparation."));
        }
        let draft = json!({"schema_version":"melissa.cli-preparation/v1","goal":goal,"workspace":root,"source_digest":snap.digest,"source_files":snap.files.len(),"adaptive_plan":adaptive,"method":"offline source inspection and explicit intent rules; not an LLM plan","ready_to_create":ready,"ready_to_record_project_task":true,"recipe":recipe,"approval_required_before_execution":true,"model_calls":0,"missing_acceptance_invented":false});
        let body = if ready {
            bytes(&draft["recipe"])?
        } else {
            bytes(&draft)?
        };
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&out)?;
        file.write_all(&body)?;
        file.sync_all()?;
        Ok(
            json!({"command":"task prepare","out":out,"sha":digest(&body),"preparation":draft,"next":if ready{"Review the recipe, then task new --recipe and the normal source-bound approval."}else{"In the terminal session use /create to record the reviewed project brief without a private oracle. General execution/environment binding follows in Phase 3."}}),
        )
    }
    pub fn replan(&self, id: &str, recipe: Recipe) -> Result<Value> {
        recipe.validate()?;
        let mut store = self.store(id)?;
        let t = store.journal().task(id)?;
        if t.record.state != melissa_core_contracts::TaskState::Running
            || self.live(id)?.phase != "paused"
        {
            return Err(Error::Conflict(
                "Yeniden planlama yalnız güvenli pause kaydında yapılır.",
            ));
        }
        let e = self.execution(&store, id)?.ok_or(Error::Store)?;
        let mut permitted = e.recipe.clone();
        permitted.agents = recipe.agents.clone();
        if bytes(&permitted)? != bytes(&recipe)? {
            return Err(Error::Denied(
                "Yeniden planlama yalnız uzman adımlarını değiştirebilir; kabul, dosyalar, süre ve bütçe sabit.",
            ));
        }
        if recipe
            .agents
            .iter()
            .any(|a| !t.contract.selected_profile_ids.contains(&a.profile))
        {
            return Err(Error::Denied("Yeni uzman ilk görev yetkisinde değil."));
        }
        let previous = self.live(id)?;
        let repo = self.source(&self.workspace(&store, id)?)?;
        let snapshot = crate::app::decode(
            &store.get_artifact(id, &format!("cli:paused-source:{}", previous.session))?,
        )?;
        repo.validate_snapshot(&snapshot, &AtomicBool::new(false))?;
        let budget = store.journal().model_budget(id)?;
        if budget.active_requests != 0
            || budget.unknown_requests != 0
            || t.record.unknown_invocation_id.is_some()
        {
            return Err(Error::Reconciliation(
                "Belirsiz işlem varken plan değişmez.",
            ));
        }
        let source = t.contract.input_snapshot_digest.clone();
        let policy = e.settings_digest.clone();
        let mut scheduler = melissa_workflow::scheduler::Scheduler::open(
            store.connect()?,
            id.into(),
            "cli-workflow-v1".into(),
            Arc::new(move || {
                Ok(melissa_workflow::scheduler::Freshness {
                    workspace_digest: source.clone(),
                    policy_digest: policy.clone(),
                })
            }),
            Arc::new(AtomicBool::new(false)),
        )
        .map_err(|_| Error::Conflict("Plan durumu açılamadı."))?;
        let old = scheduler.state().map_err(|_| Error::Store)?;
        let mut revised = e.clone();
        revised.recipe = recipe;
        let mut plan = crate::engine::plan(&t.contract, &revised)?;
        plan.revision = old.plan.revision.checked_add(1).ok_or(Error::Store)?;
        plan.parent_digest = Some(old.plan.digest().map_err(|_| Error::Store)?);
        keep_admitted_instructions(&old, &mut plan);
        plan.validate(&t.contract, &crate::resources::catalog()?)
            .map_err(|_| Error::Denied("Yeni plan görev yetkisi dışında."))?;
        preserve_started(&old, &plan)?;
        {
            let sequence = next_revision_sequence(&store, id)?;
            let cp = format!("cli:pending-plan:{sequence}");
            let artifact = format!("{cp}:bytes");
            let raw = bytes(&plan)?;
            crate::app::put(&mut store, id, &artifact, "workflow.pending-revision", &raw)?;
            let version = store.journal().task(id)?.record.version;
            store.append_checkpoint(
                &cp,
                id,
                version,
                &artifact,
                serde_json::json!({"digest":digest(&raw)}),
            )?;
            let activated = activate_pending(&mut store, id, &mut scheduler)?;
            Ok(
                json!({"command":"task replan","task_id":id,"status":if activated{"applied"}else{"queued_until_current_step_completes"},"revision":plan.revision,"model_calls":0,"budget_reset":false,"acceptance_changed":false,"plan":plan}),
            )
        }
    }
}

fn keep_admitted_instructions(
    old: &melissa_workflow::scheduler::State,
    plan: &mut melissa_workflow::plan::Plan,
) {
    for node in &mut plan.nodes {
        let previous = if let Some(ledger) = &old.adaptive {
            ledger.templates.values().find(|template| {
                let mut input = node.literal_input.clone();
                input["instructions"] = template.literal_input["instructions"].clone();
                template.profile == node.profile && template.literal_input == input
            })
        } else if old.slots.get(&node.id).is_some_and(|slot| {
            matches!(
                slot.phase,
                melissa_workflow::scheduler::Phase::Candidate
                    | melissa_workflow::scheduler::Phase::Leased
            )
        }) {
            old.plan
                .nodes
                .iter()
                .find(|previous| previous.id == node.id)
        } else {
            None
        };
        if let Some(previous) = previous {
            node.literal_input["instructions"] = previous.literal_input["instructions"].clone();
        }
    }
}

fn preserve_started(
    old: &melissa_workflow::scheduler::State,
    plan: &melissa_workflow::plan::Plan,
) -> Result {
    for node in &old.plan.nodes {
        if matches!(
            old.slots[&node.id].phase,
            melissa_workflow::scheduler::Phase::Candidate
                | melissa_workflow::scheduler::Phase::Leased
        ) && plan.nodes.iter().find(|n| n.id == node.id) != Some(node)
        {
            return Err(Error::Denied(
                "Çalışan veya tamamlanan uzman adımı değiştirilemez/silinemez.",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod replan_tests {
    use super::*;
    use melissa_workflow::{
        plan::Plan,
        scheduler::{Phase, State},
    };

    #[test]
    fn legacy_adaptive_templates_admit_revised_and_additional_pending_steps() {
        use melissa_core_contracts::{TaskState, wire::TaskContractV1};
        use melissa_local_store::{LocalStore, StoreKey};
        use melissa_workflow::scheduler::{Freshness, Scheduler};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        #[cfg(not(windows))]
        let root = root.canonicalize().unwrap();
        let mut task: TaskContractV1 = serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/code01/wire/task-contract-v1.json"
        ))
        .unwrap();
        task.selected_profile_ids = vec![
            "melissa.code.implementer".into(),
            "melissa.code.test-engineer".into(),
            "melissa.code.reviewer".into(),
        ];
        task.allowed_effects = vec!["workspace.read".into()];
        let recipe = serde_json::from_value(json!({"schema_version":"melissa.cli-recipe/v1",
            "agents":task.selected_profile_ids.iter().map(|profile| json!({"profile":profile,"goal":"inspect","write":false})).collect::<Vec<_>>(),
            "writable_paths":[],"protected_paths":[],"language":"python","entry":"main.py",
            "negative_workspace":root.join("negative"),"checks":[{"id":"one","input":1,"expected":1},{"id":"two","input":2,"expected":2}],
            "max_cost_micro_usd":1000000,"wall_time_ms":60000})).unwrap();
        let mut execution = crate::recipe::Execution {
            recipe,
            settings_digest: digest(b"policy"),
            expires_at_ms: crate::app::now().unwrap() + 60000,
            negative_files: Default::default(),
            runtime_digest: None,
        };
        let mut plan = crate::engine::plan(&task, &execution).unwrap();
        for node in &mut plan.nodes {
            node.literal_input["instructions"] = json!(format!("legacy {}", node.profile));
        }
        let fresh = Freshness {
            workspace_digest: plan.workspace_digest.clone(),
            policy_digest: plan.policy_digest.clone(),
        };
        let mut store = LocalStore::open(
            &root.join("state"),
            StoreKey::from_bytes([82; 32]),
            Default::default(),
        )
        .unwrap();
        store.journal_mut().create_task(&task).unwrap();
        for state in [
            TaskState::Planned,
            TaskState::Queued,
            TaskState::Leased,
            TaskState::Running,
        ] {
            let version = store.journal().task(&task.task_id).unwrap().record.version;
            store
                .transition_task(&task.task_id, version, state, None)
                .unwrap();
        }
        let catalog = crate::resources::catalog().unwrap();
        let mut scheduler = Scheduler::create(
            store,
            plan,
            &catalog,
            Arc::new(move || Ok(fresh.clone())),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let old = scheduler.state().unwrap();
        let budget = scheduler
            .store()
            .journal()
            .model_budget(&task.task_id)
            .unwrap();
        execution.recipe.agents[1].goal = "revised checks".into();
        execution
            .recipe
            .agents
            .insert(2, execution.recipe.agents[1].clone());
        let mut revised = crate::engine::plan(&task, &execution).unwrap();
        revised.revision = old.plan.revision + 1;
        revised.parent_digest = Some(old.plan.digest().unwrap());
        assert!(scheduler.revise(revised.clone(), &catalog).is_err());
        let mut invalid = revised.clone();
        invalid.nodes[1].literal_input["protected_paths"] = json!(["changed"]);
        keep_admitted_instructions(&old, &mut invalid);
        assert!(scheduler.revise(invalid, &catalog).is_err());
        keep_admitted_instructions(&old, &mut revised);
        let after = scheduler.revise(revised, &catalog).unwrap();
        assert_eq!(after.plan.nodes.len(), 4);
        assert_eq!(after.deadline_ms, old.deadline_ms);
        assert_eq!(
            after.adaptive.unwrap().templates,
            old.adaptive.unwrap().templates
        );
        assert_eq!(
            scheduler
                .store()
                .journal()
                .model_budget(&task.task_id)
                .unwrap(),
            budget
        );
    }

    #[test]
    fn legacy_started_instructions_are_frozen_without_hiding_node_changes() {
        let node = json!({"id":"step-0","profile":"implementer","goal":"original",
            "after":[],"bindings":[],"literal_input":{"instructions":"legacy"},
            "tools":["fs.read"],"effects":["workspace.read"],"resources":["source"],
            "authorizations":[],"claims":[],"output_schema":{},"skills":[]});
        let mut pending = node.clone();
        pending["id"] = json!("step-1");
        let plan: Plan =
            serde_json::from_value(json!({"schema_version":"melissa.workflow-plan/v1",
            "id":"cli-workflow-v1","task":"task","revision":1,"parent_digest":null,
            "workspace_digest":"source","policy_digest":"policy","mode":"conditional_delegation",
            "delegation_reason":"test","max_parallel":1,"max_no_progress":8,
            "nodes":[node,pending],"outputs":["step-1"]}))
            .unwrap();
        for phase in [Phase::Leased, Phase::Candidate] {
            let old: State = serde_json::from_value(json!({"schema_version":"fixture","sequence":0,
                "previous_digest":null,"plan":plan,"slots":{
                    "step-0":{"phase":phase,"generation":0,"lease":null,"output":null,"native_run_ref":null},
                    "step-1":{"phase":"pending","generation":0,"lease":null,"output":null,"native_run_ref":null}},
                "cancel_requested":false,"cancellation_settled":false,"no_progress":0,
                "last_clock_ms":0,"started_at_ms":0,"deadline_ms":1000,"reason":"fixture"})).unwrap();
            let mut revised = plan.clone();
            for node in &mut revised.nodes {
                node.literal_input["instructions"] = json!("new compiler guidance");
            }
            revised.nodes[1].goal = "revised pending work".into();
            keep_admitted_instructions(&old, &mut revised);
            preserve_started(&old, &revised).unwrap();
            assert_eq!(revised.nodes[0], old.plan.nodes[0]);
            assert_eq!(
                revised.nodes[1].literal_input["instructions"],
                "new compiler guidance"
            );
            for field in ["goal", "profile", "tools", "effects", "claims"] {
                let mut changed = serde_json::to_value(&revised).unwrap();
                changed["nodes"][0][field] = match field {
                    "goal" | "profile" => json!("changed"),
                    "tools" | "effects" => json!(["changed"]),
                    "claims" => json!([{"resource":"source","path":"/","access":"write"}]),
                    _ => continue,
                };
                let mut changed: Plan = serde_json::from_value(changed).unwrap();
                keep_admitted_instructions(&old, &mut changed);
                assert!(preserve_started(&old, &changed).is_err(), "{field}");
            }
            revised.nodes.remove(0);
            keep_admitted_instructions(&old, &mut revised);
            assert!(preserve_started(&old, &revised).is_err());
        }
    }
}
fn next_revision_sequence(store: &melissa_local_store::LocalStore, task: &str) -> Result<u64> {
    let mut n = 0;
    loop {
        match store
            .journal()
            .checkpoint(&format!("cli:pending-plan:{n}"), task)
        {
            Ok(_) => n += 1,
            Err(melissa_local_store::StoreError::NotFound(_)) => return Ok(n),
            Err(e) => return Err(e.into()),
        }
    }
}
pub(crate) fn activate_pending(
    store: &mut melissa_local_store::LocalStore,
    task: &str,
    scheduler: &mut melissa_workflow::scheduler::Scheduler,
) -> Result<bool> {
    let next = next_revision_sequence(store, task)?;
    if next == 0 {
        return Ok(false);
    }
    let n = next - 1;
    if store
        .journal()
        .checkpoint(&format!("cli:discarded-plan:{n}"), task)
        .is_ok()
    {
        return Ok(false);
    }
    let applied = format!("cli:applied-plan:{n}");
    match store.journal().checkpoint(&applied, task) {
        Ok(_) => return Ok(false),
        Err(melissa_local_store::StoreError::NotFound(_)) => {}
        Err(e) => return Err(e.into()),
    }
    let cp = store
        .journal()
        .checkpoint(&format!("cli:pending-plan:{n}"), task)?;
    let raw = store.get_artifact(task, &cp.artifact_id)?;
    if cp.data["digest"] != digest(&raw) {
        return Err(Error::Store);
    }
    let plan: melissa_workflow::plan::Plan = crate::app::decode(&raw)?;
    let state = scheduler.state().map_err(|_| Error::Store)?;
    if state
        .slots
        .values()
        .any(|s| s.phase == melissa_workflow::scheduler::Phase::Leased)
    {
        return Ok(false);
    }
    if state.plan != plan {
        preserve_started(&state, &plan)?;
        scheduler
            .revise(plan, &crate::resources::catalog()?)
            .map_err(|_| {
                Error::Conflict("Bekleyen plan güncel kapsamla uyuşmadı; otomatik değiştirilmedi.")
            })?;
    }
    let version = store.journal().task(task)?.record.version;
    store.append_checkpoint(
        &applied,
        task,
        version,
        &cp.artifact_id,
        json!({"digest":digest(&raw),"budget_reset":false}),
    )?;
    Ok(true)
}

// Source-visible import candidates are complexity hints, not semantic proof.
fn dependency_hints(
    repo: &Repository,
    snapshot: &melissa_repository::RepositorySnapshot,
    affected: &BTreeSet<String>,
) -> Result<Value> {
    let cancel = AtomicBool::new(false);
    let mut files = snapshot.files.iter().collect::<Vec<_>>();
    files.sort_by_key(|f| (!affected.contains(&f.path), &f.path));
    let mut facts = std::collections::BTreeMap::new();
    let mut bytes_read = 0usize;
    let mut nodes_left = melissa_repository::polyglot::MAX_SYNTAX_NODES;
    let mut incomplete = false;
    for file in files {
        if melissa_repository::discovery::language(&file.path).is_none() {
            continue;
        }
        if facts.len() >= 512 || nodes_left == 0 {
            incomplete = true;
            break;
        }
        let (version, bytes) = repo.read(&file.path)?;
        if version.digest != file.digest {
            return Err(Error::Conflict("Dependency source changed."));
        }
        bytes_read = bytes_read.saturating_add(bytes.len());
        if bytes_read > 32 * 1024 * 1024 {
            incomplete = true;
            break;
        }
        let parsed = match melissa_repository::polyglot::parse_index_file(
            &version, &bytes, &cancel, nodes_left,
        ) {
            Err(melissa_repository::Error::Quota(_)) => {
                incomplete = true;
                break;
            }
            result => result?,
        };
        match parsed {
            Some(parsed) => {
                nodes_left = nodes_left.saturating_sub(parsed.nodes);
                if let Some(graph) = parsed.graph {
                    incomplete |= graph.truncated || graph.syntax_error;
                    facts.insert(file.path.clone(), graph);
                } else {
                    incomplete = true;
                }
            }
            None => {
                incomplete = true;
            }
        }
    }
    let graph =
        melissa_repository::graph::Graph::new(facts.iter().map(|(p, f)| (p.as_str(), f)).collect());
    let edges = graph
        .imports
        .values()
        .flatten()
        .filter(|edge| {
            edge.dependency.is_some_and(|dep| {
                dep != edge.path && (affected.contains(edge.path) || affected.contains(dep))
            })
        })
        .count();
    let unresolved = graph
        .imports
        .values()
        .flatten()
        .filter(|edge| affected.contains(edge.path) && edge.dependency.is_none())
        .count();
    Ok(
        json!({"source_digest":snapshot.digest,"cross_file_edges":edges,"unresolved_imports":unresolved,"parsed_files":facts.len(),"incomplete":incomplete,"evidence_kind":"bounded_source_import_candidates","semantic_resolution":false}),
    )
}
