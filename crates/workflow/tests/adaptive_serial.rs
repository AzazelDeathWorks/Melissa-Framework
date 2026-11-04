#![allow(dead_code)]
include!("common/gateway.rs");
use melissa_workflow::{plan::*, runtime::*, scheduler::*};
struct NoTools;
impl ToolDispatcher for NoTools {
    fn definitions(&self) -> Vec<BoundTool> {
        vec![]
    }
    fn validate_call(&self, _: &ToolCall) -> Result<()> {
        Err(GatewayError::invalid("none"))
    }
    fn execute<'a>(
        &'a mut self,
        _: ToolCall,
        _: ToolContext,
        _: Arc<AtomicBool>,
    ) -> Pin<Box<dyn Future<Output = Result<ToolExecution>> + Send + 'a>> {
        Box::pin(async { Err(GatewayError::invalid("none")) })
    }
}
fn setup() -> (tempfile::TempDir, Arc<Mutex<Scheduler>>) {
    setup_with_writer(false)
}
fn setup_with_writer(writer: bool) -> (tempfile::TempDir, Arc<Mutex<Scheduler>>) {
    setup_until(writer, u64::MAX)
}
fn setup_until(writer: bool, deadline: u64) -> (tempfile::TempDir, Arc<Mutex<Scheduler>>) {
    let d = tempfile::tempdir().unwrap();
    let store = store(&d.path().join("s"), 1000000);
    let task = store.journal().task(TASK).unwrap().contract;
    let mut previous = None;
    let mut nodes:Vec<Node>=[("implementation","melissa.code.implementer"),("tests","melissa.code.test-engineer"),("review","melissa.code.reviewer")].into_iter().map(|(id,profile)|{
        let after=previous.iter().cloned().collect();let bindings=vec![];previous=Some(id.to_string());
        Node{id:id.into(),profile:profile.into(),goal:format!("Perform bounded stage {id}"),after,bindings,literal_input:json!({}),tools:BTreeSet::new(),effects:BTreeSet::from(["workspace.read".into()]),resources:BTreeSet::from([task.workspace_ref.clone()]),authorizations:BTreeSet::new(),claims:vec![],output_schema:json!({"type":"object","properties":{"value":{"type":"integer"}},"required":["value"],"additionalProperties":false}),skills:vec![]}
    }).collect();
    if writer {
        nodes[0]
            .effects
            .insert("workspace.apply_patch_scoped".into());
        nodes[0].claims.push(Claim {
            resource: task.workspace_ref.clone(),
            path: "main.py".into(),
            access: Access::Write,
        });
    }
    let p = Plan {
        schema_version: "melissa.workflow-plan/v1".into(),
        id: "cooperation".into(),
        task: TASK.into(),
        revision: 1,
        parent_digest: None,
        workspace_digest: task.input_snapshot_digest.clone(),
        policy_digest: melissa_workflow::digest(b"policy"),
        mode: Mode::AdaptiveSerial,
        delegation_reason: Some(
            "local integration evaluation of three existing role profiles".into(),
        ),
        max_parallel: 1,
        max_no_progress: 3,
        nodes,
        outputs: BTreeSet::from(["review".into()]),
    };
    let fresh = Freshness {
        workspace_digest: p.workspace_digest.clone(),
        policy_digest: p.policy_digest.clone(),
    };
    let catalog =
        Catalog::load_definitions(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
    let scheduler = Scheduler::create_until(
        store,
        p,
        &catalog,
        Arc::new(move || Ok(fresh.clone())),
        Arc::new(AtomicBool::new(false)),
        deadline,
    )
    .unwrap();
    (d, Arc::new(Mutex::new(scheduler)))
}
async fn driver(
    owner: Arc<Mutex<Scheduler>>,
    responses: Vec<HttpPlan>,
) -> (Runtime, Arc<HttpFixture>) {
    let http = Arc::new(http_fixture(responses).await);
    let s = owner.clone();
    let server = http.clone();
    let runtime = Runtime {
        scheduler: owner,
        lease_ms: 30000,
        factory: Arc::new(move |packet| {
            let store = s.lock().unwrap().store().connect().unwrap();
            let mut sp = spec(&packet.lease.run);
            sp.profile_id = packet.node.profile.clone();
            sp.user_message = serde_json::to_string(packet).unwrap();
            sp.output_schema = Some(packet.node.output_schema.clone());
            Ok(PreparedRun {
                gateway: gateway(store, &server, policy()),
                spec: sp,
                dispatcher: Box::new(NoTools),
            })
        }),
    };
    (runtime, http)
}

use melissa_workflow::adaptive::{self, Complexity, Proposal, Signals, StepProposal};
fn catalog() -> Catalog {
    Catalog::load_definitions(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap()
}
fn proposal(s: &State, steps: &[&str]) -> Proposal {
    Proposal {
        expected_plan_digest: s.plan.digest().unwrap(),
        source_version: s.adaptive.as_ref().unwrap().source_version.clone(),
        reason: "Observed dependency requires another bounded implementation pass".into(),
        remaining_steps: steps
            .iter()
            .map(|id| StepProposal {
                template: (*id).into(),
                goal: format!("Perform {id} with existing authority"),
            })
            .collect(),
    }
}
fn reopen(s: &Scheduler) -> Scheduler {
    let state = s.state().unwrap();
    let fresh = Freshness {
        workspace_digest: state.plan.workspace_digest,
        policy_digest: state.plan.policy_digest,
    };
    Scheduler::open(
        s.store().connect().unwrap(),
        TASK.into(),
        "cooperation".into(),
        Arc::new(move || Ok(fresh.clone())),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap()
}
fn response() -> HttpPlan {
    HttpPlan::sse(text_response(r#"{"value":1}"#, false, true))
}

#[test]
fn complexity_and_registry_selection_cover_all_classes_and_missing_roles() {
    let mut signals = Signals {
        repository_files: 2,
        affected_files: 1,
        affected_modules: 1,
        cross_module_dependencies: 0,
        test_files: 1,
        high_risk: false,
        architecture_change: false,
        compiler_failure: false,
        estimated_cost_micro_usd: None,
        authorized_cost_micro_usd: None,
    };
    assert_eq!(adaptive::complexity(&signals), Complexity::Simple);
    let c = catalog();
    assert_eq!(
        adaptive::specialists(&signals, "implementer", &c).unwrap(),
        vec![
            "melissa.code.implementer",
            "melissa.code.test-engineer",
            "melissa.code.reviewer"
        ]
    );
    signals.cross_module_dependencies = 1;
    assert_eq!(adaptive::complexity(&signals), Complexity::Moderate);
    signals.architecture_change = true;
    assert_eq!(adaptive::complexity(&signals), Complexity::Complex);
    signals.affected_files = 65;
    assert_eq!(adaptive::complexity(&signals), Complexity::LongHorizon);
    signals.high_risk = true;
    signals.compiler_failure = true;
    assert_eq!(adaptive::complexity(&signals), Complexity::HighRisk);
    let roles = adaptive::specialists(&signals, "implementer", &c).unwrap();
    assert!(roles.iter().any(|p| p == "melissa.code.debugger"));
    assert!(roles.iter().any(|p| p == "melissa.code.architect"));
    assert!(roles.iter().any(|p| p == "melissa.code.security-reviewer"));
    assert!(adaptive::specialists(&signals, "unavailable", &c).is_err());
    let mut c = c;
    c.profiles.remove("melissa.code.test-engineer");
    assert!(adaptive::specialists(&signals, "implementer", &c).is_err());
}

#[tokio::test]
async fn real_native_serial_handoffs_reopen_and_keep_claims_separate_from_acceptance() {
    let (_dir, owner) = setup();
    let (rt, http) = driver(owner.clone(), vec![response(), response(), response()]).await;
    rt.run("implementation").await.unwrap();
    let changed = melissa_workflow::digest(b"changed source");
    {
        let mut s = owner.lock().unwrap();
        s.observe_source(changed.clone(), BTreeSet::from(["main.py".into()]))
            .unwrap();
        *s = reopen(&s);
    }
    rt.run("tests").await.unwrap();
    {
        let mut s = owner.lock().unwrap();
        *s = reopen(&s);
    }
    rt.run("review").await.unwrap();
    let s = owner.lock().unwrap();
    assert_eq!(s.supervisor().unwrap()["decision"], "protected_qa_ready");
    let state = s.state().unwrap();
    let ledger = state.adaptive.unwrap();
    assert_eq!(ledger.completed_milestones.len(), 3);
    assert!(ledger.accepted_artifacts.is_empty());
    assert_eq!(ledger.candidate_artifacts.len(), 3);
    assert_eq!(
        s.store().journal().task(TASK).unwrap().record.state,
        TaskState::Running
    );
    assert_eq!(
        s.store()
            .journal()
            .model_budget(TASK)
            .unwrap()
            .known_input_tokens,
        36
    );
    let bodies = http.bodies.lock().unwrap();
    let handoff: Value = serde_json::from_str(
        bodies[2]["input"][0]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(handoff["bound_inputs"], json!({}));
    assert_eq!(handoff["specialist_handoff"]["source_version"], changed);
    assert_eq!(
        handoff["specialist_handoff"]["completed_work"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(handoff["specialist_handoff"]["artifact_refs"], json!([]));
    assert!(handoff["specialist_handoff"].get("transcript").is_none());
    assert_eq!(http.requests.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn active_run_excludes_a_second_specialist_even_from_another_scheduler() {
    let (_dir, owner) = setup();
    let (rt, http) = driver(
        owner.clone(),
        vec![HttpPlan {
            status: 200,
            body: text_response(r#"{"value":1}"#, false, true),
            delay: Duration::from_millis(250),
        }],
    )
    .await;
    let work = rt.clone();
    let running = tokio::spawn(async move { work.run("implementation").await });
    for _ in 0..200 {
        if http.requests.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(http.requests.load(Ordering::SeqCst), 1);
    let mut other = reopen(&owner.lock().unwrap());
    assert!(other.acquire("tests", "second", 30000).is_err());
    assert!(other.acquire("implementation", "duplicate", 30000).is_err());
    assert_eq!(other.supervisor().unwrap()["active_specialists"], 1);
    running.await.unwrap().unwrap();
}

#[tokio::test]
async fn proposal_waits_for_boundary_survives_restart_and_cannot_reset_budget_or_deadline() {
    let (_dir, owner) = setup();
    let (rt, http) = driver(
        owner.clone(),
        vec![response(), response(), response(), response()],
    )
    .await;
    let (packet, old, proposed) = {
        let mut s = owner.lock().unwrap();
        let old = s.state().unwrap();
        let packet = s.acquire("implementation", "host", 30000).unwrap();
        let proposed = proposal(&s.state().unwrap(), &["implementation", "tests", "review"]);
        s.propose(&packet.lease, proposed.clone()).unwrap();
        assert!(
            s.apply_proposal(&old.plan.workspace_digest, &catalog())
                .is_err()
        );
        assert!(s.propose(&packet.lease, proposed.clone()).is_err());
        *s = reopen(&s);
        (packet, old, proposed)
    };
    rt.resume("implementation").await.unwrap();
    let next = {
        let mut s = owner.lock().unwrap();
        assert!(
            s.apply_proposal(&proposed.source_version, &catalog())
                .unwrap()
        );
        let next = s.state().unwrap();
        assert_eq!(next.deadline_ms, old.deadline_ms);
        assert_eq!(next.plan.nodes[0], packet.node);
        assert_eq!(next.adaptive.as_ref().unwrap().revision_count, 1);
        assert!(
            s.store()
                .journal()
                .model_budget(TASK)
                .unwrap()
                .known_input_tokens
                > 0
        );
        let mut repeat = proposed.clone();
        repeat.expected_plan_digest = next.plan.digest().unwrap();
        assert!(adaptive::revise(&next, &repeat).is_err());
        next
    };
    for node in next.plan.nodes.iter().skip(1) {
        rt.run(&node.id).await.unwrap();
    }
    assert_eq!(http.requests.load(Ordering::SeqCst), 4);
    assert_eq!(owner.lock().unwrap().outputs().unwrap().len(), 1);
}

#[tokio::test]
async fn stale_source_proposal_is_discarded_without_replay_or_cost_reset() {
    let (_dir, owner) = setup();
    let (rt, http) = driver(owner.clone(), vec![response()]).await;
    {
        let mut s = owner.lock().unwrap();
        let packet = s.acquire("implementation", "host", 30000).unwrap();
        let p = proposal(&s.state().unwrap(), &["tests", "review"]);
        s.propose(&packet.lease, p).unwrap();
    }
    rt.resume("implementation").await.unwrap();
    let mut s = owner.lock().unwrap();
    let source = melissa_workflow::digest(b"source edited after proposal");
    s.observe_source(source.clone(), BTreeSet::from(["main.py".into()]))
        .unwrap();
    assert!(!s.apply_proposal(&source, &catalog()).unwrap());
    let state = s.state().unwrap();
    assert_eq!(state.plan.revision, 1);
    assert_eq!(state.no_progress, 1);
    assert!(state.adaptive.unwrap().pending_proposal.is_none());
    assert_eq!(http.requests.load(Ordering::SeqCst), 1);
}

#[test]
fn unauthorized_profiles_authority_changes_bad_order_and_plan_versions_are_rejected() {
    let (_dir, owner) = setup();
    let mut s = owner.lock().unwrap();
    let packet = s.acquire("implementation", "host", 30000).unwrap();
    let state = s.state().unwrap();
    for steps in [
        &["unknown", "tests", "review"][..],
        &["review"][..],
        &["tests", "implementation", "review"][..],
    ] {
        let p = proposal(&state, steps);
        // Mark the initial implementation template as a writer to check final testing order.
        let mut writer_state = state.clone();
        let template = writer_state
            .adaptive
            .as_mut()
            .unwrap()
            .templates
            .get_mut("implementation")
            .unwrap();
        template.claims = vec![Claim {
            resource: packet.parent_task.workspace_ref.clone(),
            path: "main.py".into(),
            access: Access::Write,
        }];
        assert!(adaptive::revise(&writer_state, &p).is_err());
    }
    let mut p = proposal(&state, &["tests", "review"]);
    p.expected_plan_digest = melissa_workflow::digest(b"old");
    assert!(s.propose(&packet.lease, p).is_err());
    let bad = json!({"expected_plan_digest":state.plan.digest().unwrap(),"source_version":state.plan.workspace_digest,"reason":"broaden","remaining_steps":[{"template":"tests","goal":"test","tools":["fs.patch"]}]});
    assert!(serde_json::from_value::<Proposal>(bad).is_err());
    assert!(s.record_acceptance("fake").is_err());
}

#[tokio::test]
async fn failed_native_attempts_and_check_receipts_survive_reopen_and_bound_repeated_failures() {
    let (_dir, owner) = setup();
    let (rt, _http) = driver(
        owner.clone(),
        vec![HttpPlan::sse(text_response("cannot continue", true, true))],
    )
    .await;
    {
        let mut s = owner.lock().unwrap();
        let packet = s.acquire("implementation", "host", 30000).unwrap();
        for _ in 0..3 {
            s.observe_check(
                &packet.lease,
                "code_tests",
                &packet.parent_task.input_snapshot_digest,
                false,
                "assertion failed",
            )
            .unwrap();
        }
        *s = reopen(&s);
    }
    rt.resume("implementation").await.unwrap();
    let mut s = owner.lock().unwrap();
    let state = s.state().unwrap();
    assert_eq!(state.slots["implementation"].phase, Phase::Failed);
    let ledger = state.adaptive.unwrap();
    assert_eq!(ledger.failed_attempts.len(), 1);
    assert_eq!(ledger.repeated_failures.values().sum::<u32>(), 3);
    assert!(ledger.accepted_artifacts.is_empty());
    assert_eq!(s.supervisor().unwrap()["decision"], "no_progress_stop");
    assert!(s.acquire("tests", "worker", 30000).is_err());
}

#[test]
fn source_bound_check_facts_invalidate_on_change_and_legacy_packets_remain_readable() {
    let (_dir, owner) = setup();
    let mut s = owner.lock().unwrap();
    let packet = s.acquire("implementation", "host", 30000).unwrap();
    s.observe_check(
        &packet.lease,
        "code_tests",
        &packet.parent_task.input_snapshot_digest,
        true,
        "passed",
    )
    .unwrap();
    let state = s.state().unwrap();
    let ledger = state.adaptive.unwrap();
    assert_eq!(
        ledger.verified_facts["successful_check:code_tests"],
        packet.parent_task.input_snapshot_digest
    );
    let mut old = serde_json::to_value(&packet).unwrap();
    old.as_object_mut().unwrap().remove("specialist_handoff");
    let decoded: WorkPacket = serde_json::from_value(old).unwrap();
    assert!(decoded.specialist_handoff.is_none());
    assert!(
        s.observe_source(melissa_workflow::digest(b"other"), BTreeSet::new())
            .is_err()
    );
}

#[test]
fn operator_revision_cannot_raise_limits_or_grant_new_tools() {
    let (_dir, owner) = setup();
    let mut s = owner.lock().unwrap();
    let state = s.state().unwrap();
    let mut revision = state.plan.clone();
    revision.revision += 1;
    revision.parent_digest = Some(state.plan.digest().unwrap());
    revision.max_no_progress += 1;
    assert!(s.revise(revision.clone(), &catalog()).is_err());
    revision.max_no_progress = state.plan.max_no_progress;
    revision.nodes[0].tools.insert("fs.read".into());
    assert!(s.revise(revision.clone(), &catalog()).is_err());
    revision.nodes[0].tools.clear();
    revision.mode = Mode::ConditionalDelegation;
    assert!(s.revise(revision, &catalog()).is_err());
    assert_eq!(s.state().unwrap().sequence, state.sequence);
}
#[tokio::test]
async fn only_authenticated_host_completion_promotes_acceptance_and_clears_stale_checks() {
    let (_dir, owner) = setup();
    let (rt, _http) = driver(owner.clone(), vec![response(), response(), response()]).await;
    {
        let mut s = owner.lock().unwrap();
        let packet = s.acquire("implementation", "host", 30000).unwrap();
        s.observe_check(
            &packet.lease,
            "code_tests",
            &packet.parent_task.input_snapshot_digest,
            true,
            "passed",
        )
        .unwrap();
    }
    rt.resume("implementation").await.unwrap();
    {
        let mut s = owner.lock().unwrap();
        s.observe_source(
            melissa_workflow::digest(b"new"),
            BTreeSet::from(["main.py".into()]),
        )
        .unwrap();
        assert!(
            !s.state()
                .unwrap()
                .adaptive
                .unwrap()
                .verified_facts
                .contains_key("successful_check:code_tests")
        );
    }
    rt.run("tests").await.unwrap();
    rt.run("review").await.unwrap();
    let mut s = owner.lock().unwrap();
    let mut store = s.store().connect().unwrap();
    let meta = store
        .put_artifact(
            &melissa_local_store::ArtifactRequest {
                task_id: TASK.into(),
                artifact_id: "host:accepted".into(),
                content_type: "application/json".into(),
                logical_type: "fixture.host-acceptance".into(),
                schema_version: "v1".into(),
                retain_until_ms: 0,
                legal_hold: false,
            },
            b"{}",
        )
        .unwrap();
    assert!(s.record_acceptance("host:accepted").is_err());
    let t = store.journal().task(TASK).unwrap();
    let selected = store
        .set_final_artifact(TASK, t.record.version, "host:accepted")
        .unwrap();
    let verifying = store
        .transition_task(TASK, selected.record.version, TaskState::Verifying, None)
        .unwrap();
    let proof = melissa_core_contracts::CompletionEvidence {
        task_id: TASK.into(),
        acceptance_digest: t.contract.acceptance_contract_digest,
        final_digest: meta.digest.clone(),
        verified_digest: meta.digest,
        verifier_run_id: "fixture-host-qa".into(),
        protected_checks_passed: true,
        actual_tool_run: true,
        raw_artifact_refs: vec!["host:accepted".into()],
    };
    store
        .transition_task(
            TASK,
            verifying.record.version,
            TaskState::Completed,
            Some(&proof),
        )
        .unwrap();
    s.record_acceptance("host:accepted").unwrap();
    let sequence = s.state().unwrap().sequence;
    s.record_acceptance("host:accepted").unwrap();
    assert_eq!(s.state().unwrap().sequence, sequence);
    let reopened = reopen(&s);
    assert_eq!(
        reopened
            .state()
            .unwrap()
            .adaptive
            .unwrap()
            .accepted_artifacts,
        vec!["host:accepted"]
    );
    assert_eq!(reopened.supervisor().unwrap()["decision"], "completed");
}

#[tokio::test]
async fn failed_specialist_can_return_as_new_serial_step_without_erasing_attempt_or_usage() {
    let (_dir, owner) = setup();
    let (rt, http) = driver(
        owner.clone(),
        vec![
            HttpPlan::sse(text_response("Need a repair pass", true, true)),
            response(),
            response(),
            response(),
        ],
    )
    .await;
    {
        let mut s = owner.lock().unwrap();
        let packet = s.acquire("implementation", "host", 30000).unwrap();
        let p = proposal(&s.state().unwrap(), &["implementation", "tests", "review"]);
        s.propose(&packet.lease, p).unwrap();
    }
    rt.resume("implementation").await.unwrap();
    let revised = {
        let mut s = owner.lock().unwrap();
        let state = s.state().unwrap();
        assert_eq!(state.slots["implementation"].phase, Phase::Failed);
        assert!(
            s.apply_proposal(&state.plan.workspace_digest, &catalog())
                .unwrap()
        );
        let next = s.state().unwrap();
        assert_eq!(next.no_progress, 1);
        assert_eq!(next.adaptive.as_ref().unwrap().failed_attempts.len(), 1);
        assert!(next.plan.nodes.iter().all(|n| n.id != "implementation"));
        next
    };
    for node in &revised.plan.nodes {
        rt.run(&node.id).await.unwrap();
    }
    let s = owner.lock().unwrap();
    assert_eq!(s.supervisor().unwrap()["decision"], "protected_qa_ready");
    assert_eq!(
        s.store()
            .journal()
            .model_budget(TASK)
            .unwrap()
            .known_input_tokens,
        48
    );
    assert_eq!(
        s.state().unwrap().adaptive.unwrap().failed_attempts.len(),
        1
    );
    assert_eq!(http.requests.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn diagnostic_repair_reuses_approved_authority_and_requires_current_retest() {
    let (_dir, owner) = setup_with_writer(true);
    let (rt, http) = driver(
        owner.clone(),
        vec![response(), response(), response(), response(), response()],
    )
    .await;
    rt.run("implementation").await.unwrap();
    {
        let mut s = owner.lock().unwrap();
        let packet = s.acquire("tests", "host", 30000).unwrap();
        s.observe_check(&packet.lease,"code_project_tests",&packet.parent_task.input_snapshot_digest,
            false,&json!({"schema_version":"melissa.sdk-result/v1","status":"failed",
                "details":{"runs":[{"id":"test_contract","status":"failed"}],
                    "diagnostics":[{"level":"error","file":"/work/main.py","line":4,"message":"assertion failed","symbol":"increment"}]}}).to_string()).unwrap();
    }
    rt.resume("tests").await.unwrap();
    let (writer, tests, review, source) = {
        let mut s = owner.lock().unwrap();
        let old = s.state().unwrap();
        let old_source = old.adaptive.as_ref().unwrap().source_version.clone();
        s.checkpoint_milestone().unwrap();
        *s = reopen(&s);
        s.validate_milestone(&old_source).unwrap();
        assert!(
            s.validate_milestone(&melissa_workflow::digest(b"changed"))
                .is_err()
        );
        assert!(s.repair_from_diagnostics(&catalog()).unwrap());
        assert!(s.validate_milestone(&old_source).is_err());
        let current = s.state().unwrap();
        assert_eq!(current.deadline_ms, old.deadline_ms);
        let ledger = current.adaptive.unwrap();
        assert_eq!(
            ledger.repairs[0].diagnostic.failed_tests,
            vec!["test_contract"]
        );
        assert_eq!(
            ledger.repairs[0].diagnostic.related_diagnostics[0].line_range,
            Some([4, 4])
        );
        assert_eq!(
            ledger.repairs[0].diagnostic.related_diagnostics[0]
                .affected_file
                .as_deref(),
            Some("main.py")
        );
        let writer = &current.plan.nodes[2];
        assert_eq!(writer.claims, old.plan.nodes[0].claims);
        (
            writer.id.clone(),
            current.plan.nodes[3].id.clone(),
            current.plan.nodes[4].id.clone(),
            melissa_workflow::digest(b"repaired source"),
        )
    };
    rt.run(&writer).await.unwrap();
    {
        let mut s = owner.lock().unwrap();
        s.observe_source(source.clone(), BTreeSet::from(["main.py".into()]))
            .unwrap();
        let packet = s.acquire(&tests, "host", 30000).unwrap();
        assert_eq!(
            packet
                .specialist_handoff
                .as_ref()
                .unwrap()
                .repair
                .as_ref()
                .unwrap()
                .diagnostic
                .failed_tests,
            vec!["test_contract"]
        );
        assert!(
            s.check_evidence(&packet.lease, "code_project_tests", &source)
                .unwrap()
                .is_none()
        );
        s.observe_check(
            &packet.lease,
            "code_project_tests",
            &source,
            true,
            r#"{"status":"passed"}"#,
        )
        .unwrap();
        assert!(
            s.check_evidence(&packet.lease, "code_project_tests", &source)
                .unwrap()
                .is_some()
        );
    }
    rt.resume(&tests).await.unwrap();
    assert!(
        !owner
            .lock()
            .unwrap()
            .repair_from_diagnostics(&catalog())
            .unwrap()
    );
    rt.run(&review).await.unwrap();
    assert_eq!(http.requests.load(Ordering::SeqCst), 5);
    assert_eq!(
        owner.lock().unwrap().supervisor().unwrap()["decision"],
        "protected_qa_ready"
    );
}

#[test]
fn approved_absolute_deadline_bounds_every_specialist_lease() {
    let deadline = melissa_execution_broker::unix_ms().unwrap() + 20_000;
    let (_d, owner) = setup_until(false, deadline);
    let mut s = owner.lock().unwrap();
    assert_eq!(s.state().unwrap().deadline_ms, deadline);
    let packet = s.acquire("implementation", "host", 30_000).unwrap();
    assert_eq!(packet.lease.expires_at_ms, deadline);
    assert_eq!(packet.specialist_handoff.unwrap().deadline_ms, deadline);
}
