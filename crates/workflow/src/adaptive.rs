//! Host-owned serial planning and evidence-backed task state.
use crate::{
    Kind, Result, error,
    plan::{Mode, Node, Plan},
    require,
    scheduler::{Phase, State},
};
use melissa_core_contracts::{catalog::Catalog, wire::TaskContractV1};
use melissa_local_store::journal::ModelBudgetSnapshot;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Complexity {
    Simple,
    Moderate,
    Complex,
    LongHorizon,
    HighRisk,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signals {
    pub repository_files: usize,
    pub affected_files: usize,
    pub affected_modules: usize,
    pub cross_module_dependencies: usize,
    pub test_files: usize,
    pub high_risk: bool,
    pub architecture_change: bool,
    pub compiler_failure: bool,
    pub estimated_cost_micro_usd: Option<u64>,
    pub authorized_cost_micro_usd: Option<u64>,
}

pub fn complexity(s: &Signals) -> Complexity {
    if s.high_risk {
        Complexity::HighRisk
    } else if s.affected_files > 64 || s.affected_modules > 8 {
        Complexity::LongHorizon
    } else if s.architecture_change
        || s.affected_files > 16
        || s.affected_modules > 2
        || s.cross_module_dependencies > 8
        || s.test_files > 64
    {
        Complexity::Complex
    } else if s.affected_files > 3
        || s.repository_files > 1000
        || s.compiler_failure
        || s.cross_module_dependencies > 0
        || s.estimated_cost_micro_usd
            .zip(s.authorized_cost_micro_usd)
            .is_some_and(|(estimate, budget)| estimate > budget / 2)
    {
        Complexity::Moderate
    } else {
        Complexity::Simple
    }
}

/// Select only executable registry roles. The host chooses the primary role
/// from source inspection and operation intent.
pub fn specialists(s: &Signals, primary: &str, catalog: &Catalog) -> Result<Vec<String>> {
    let class = complexity(s);
    let mut roles = Vec::new();
    if class != Complexity::Simple {
        roles.push("repository-analyst");
    }
    if s.architecture_change
        || matches!(class, Complexity::Complex | Complexity::LongHorizon)
        || s.affected_files > 16
        || s.affected_modules > 2
        || s.cross_module_dependencies > 8
    {
        roles.push("architect");
    }
    if s.compiler_failure && primary != "debugger" {
        roles.push("debugger");
    }
    if primary != "reviewer" && primary != "test-engineer" {
        roles.push(primary);
    }
    if primary == "debugger" {
        roles.push("implementer");
    }
    if s.high_risk {
        roles.push("security-reviewer");
    }
    roles.extend(["test-engineer", "reviewer"]);
    if roles.len() == 2 {
        roles.insert(0, "repository-analyst");
    }
    let mut result = Vec::new();
    for role in roles {
        let id = format!("melissa.code.{role}");
        let profile = catalog
            .profiles
            .get(&id)
            .ok_or_else(|| error(Kind::Authority, "required executable specialist missing"))?;
        require(
            profile
                .tool_ids
                .iter()
                .any(|tool| tool == "evidence.attach"),
            Kind::Authority,
            "specialist cannot publish evidence",
        )?;
        if !result.contains(&id) {
            result.push(id);
        }
    }
    Ok(result)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepProposal {
    pub template: String,
    pub goal: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    pub expected_plan_digest: String,
    pub source_version: String,
    pub reason: String,
    pub remaining_steps: Vec<StepProposal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Milestone {
    pub id: String,
    pub profile: String,
    pub goal: String,
    pub dependencies: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ledger {
    pub schema_version: String,
    pub goal: String,
    pub current_milestone: Option<String>,
    pub completed_milestones: Vec<String>,
    pub accepted_artifacts: Vec<String>,
    pub changed_files: BTreeSet<String>,
    pub source_version: String,
    pub verified_facts: BTreeMap<String, String>,
    pub candidate_artifacts: BTreeMap<String, String>,
    pub failed_attempts: BTreeMap<String, String>,
    pub current_diagnostics: Vec<String>,
    pub open_questions: Vec<String>,
    pub next_action: String,
    pub usage: ModelBudgetSnapshot,
    pub remaining_cost_micro_usd: u64,
    pub deadline_ms: u64,
    pub templates: BTreeMap<String, Node>,
    pub pending_proposal: Option<Proposal>,
    pub applied_proposals: BTreeSet<String>,
    pub revision_count: u32,
    pub repeated_failures: BTreeMap<String, u32>,
    pub observation_refs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateSummary {
    pub milestone: String,
    pub profile: String,
    pub summary: String,
    pub truncated: bool,
    pub claimed_source_version: Option<String>,
    pub native_evidence_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handoff {
    pub task_goal: String,
    pub current_milestone: Milestone,
    pub source_version: String,
    pub completed_work: Vec<String>,
    pub candidate_summaries: Vec<CandidateSummary>,
    pub changed_files: BTreeSet<String>,
    pub verified_facts: BTreeMap<String, String>,
    pub failed_attempts: BTreeMap<String, String>,
    pub remaining_work: Vec<Milestone>,
    pub current_diagnostics: Vec<String>,
    pub open_questions: Vec<String>,
    pub evidence_refs: BTreeMap<String, String>,
    pub artifact_refs: Vec<String>,
    pub observation_refs: BTreeMap<String, String>,
    pub next_action: String,
    pub usage: ModelBudgetSnapshot,
    pub remaining_cost_micro_usd: u64,
    pub deadline_ms: u64,
    pub plan_digest: String,
    pub allowed_templates: BTreeMap<String, String>,
}

impl Ledger {
    pub fn new(task: &TaskContractV1, plan: &Plan, deadline: u64) -> Self {
        Self {
            schema_version: "melissa.adaptive-ledger/v1".into(),
            goal: task.goal.clone(),
            current_milestone: None,
            completed_milestones: vec![],
            accepted_artifacts: vec![],
            changed_files: BTreeSet::new(),
            source_version: task.input_snapshot_digest.clone(),
            verified_facts: BTreeMap::new(),
            candidate_artifacts: BTreeMap::new(),
            failed_attempts: BTreeMap::new(),
            current_diagnostics: vec![],
            open_questions: vec![],
            next_action: "Run the first authorized specialist".into(),
            usage: ModelBudgetSnapshot::default(),
            remaining_cost_micro_usd: task.limits.max_cost_micro_usd,
            deadline_ms: deadline,
            templates: plan
                .nodes
                .iter()
                .map(|n| (n.id.clone(), n.clone()))
                .collect(),
            pending_proposal: None,
            applied_proposals: BTreeSet::new(),
            revision_count: 0,
            repeated_failures: BTreeMap::new(),
            observation_refs: BTreeMap::new(),
        }
    }
    pub fn refresh_budget(&mut self, budget: ModelBudgetSnapshot, maximum: u64) {
        self.remaining_cost_micro_usd = maximum.saturating_sub(budget.charged_cost_micro_usd);
        self.usage = budget;
    }
    pub fn handoff(&self, state: &State, node: &Node) -> Result<Handoff> {
        let milestone = |n: &Node| Milestone {
            id: n.id.clone(),
            profile: n.profile.clone(),
            goal: n.goal.clone(),
            dependencies: n.after.clone(),
        };
        Ok(Handoff {
            task_goal: self.goal.clone(),
            current_milestone: milestone(node),
            source_version: self.source_version.clone(),
            completed_work: self.completed_milestones.clone(),
            candidate_summaries: state
                .plan
                .nodes
                .iter()
                .filter_map(|n| {
                    let slot = &state.slots[&n.id];
                    if slot.phase != Phase::Candidate {
                        return None;
                    }
                    let output = slot.output.as_ref()?;
                    let text = output
                        .get("summary")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| output.to_string());
                    let truncated = text.len() > 2048;
                    let mut end = text.len().min(2048);
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    Some(CandidateSummary {
                        milestone: n.id.clone(),
                        profile: n.profile.clone(),
                        summary: text[..end].into(),
                        truncated,
                        claimed_source_version: output
                            .get("source_digest")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned),
                        native_evidence_ref: slot.native_run_ref.clone()?,
                    })
                })
                .collect(),
            changed_files: self.changed_files.clone(),
            verified_facts: self.verified_facts.clone(),
            failed_attempts: self.failed_attempts.clone(),
            remaining_work: state
                .plan
                .nodes
                .iter()
                .filter(|n| state.slots[&n.id].phase == Phase::Pending)
                .map(milestone)
                .collect(),
            current_diagnostics: self.current_diagnostics.clone(),
            open_questions: self.open_questions.clone(),
            evidence_refs: self.candidate_artifacts.clone(),
            artifact_refs: self.accepted_artifacts.clone(),
            observation_refs: self.observation_refs.clone(),
            next_action: node.goal.clone(),
            usage: self.usage.clone(),
            remaining_cost_micro_usd: self.remaining_cost_micro_usd,
            deadline_ms: self.deadline_ms,
            plan_digest: state.plan.digest()?,
            allowed_templates: self
                .templates
                .iter()
                .map(|(id, node)| (id.clone(), node.profile.clone()))
                .collect(),
        })
    }
}

/// Construct a revision exclusively from the initial authority templates.
/// Completed and currently active milestones are preserved byte for byte.
pub fn revise(state: &State, proposal: &Proposal) -> Result<Plan> {
    let ledger = state
        .adaptive
        .as_ref()
        .ok_or_else(|| error(Kind::Invalid, "adaptive workflow required"))?;
    require(
        state.plan.mode == Mode::AdaptiveSerial
            && proposal.expected_plan_digest == state.plan.digest()?,
        Kind::Stale,
        "proposal binds an obsolete plan",
    )?;
    require(
        crate::plan::hash(&proposal.source_version)
            && !proposal.reason.trim().is_empty()
            && proposal.reason.len() <= 4096
            && !proposal.remaining_steps.is_empty()
            && proposal.remaining_steps.len() <= 32
            && ledger.revision_count < state.plan.max_no_progress,
        Kind::Invalid,
        "planner proposal or revision bound",
    )?;
    let signature = crate::digest(&serde_json::to_vec(&proposal.remaining_steps)?);
    require(
        !ledger.applied_proposals.contains(&signature),
        Kind::Conflict,
        "repeated planner proposal",
    )?;
    let mut plan = state.plan.clone();
    plan.nodes.retain(|node| {
        matches!(
            state.slots[&node.id].phase,
            Phase::Candidate | Phase::Leased
        )
    });
    let mut previous = plan.nodes.last().map(|n| n.id.clone());
    let revision = plan
        .revision
        .checked_add(1)
        .ok_or_else(|| error(Kind::Invalid, "revision overflow"))?;
    for (index, step) in proposal.remaining_steps.iter().enumerate() {
        require(
            !step.goal.trim().is_empty() && step.goal.len() <= 16384,
            Kind::Invalid,
            "invalid proposed milestone",
        )?;
        let mut node = ledger
            .templates
            .get(&step.template)
            .ok_or_else(|| error(Kind::Authority, "proposal selects an unauthorized template"))?
            .clone();
        node.id = format!("revision-{revision}-step-{index}");
        node.goal = step.goal.clone();
        node.after = previous.iter().cloned().collect();
        // Structured handoff carries evidence across roles. No transcript binding.
        node.bindings.clear();
        previous = Some(node.id.clone());
        plan.nodes.push(node);
    }
    require(
        plan.nodes.len() <= 128,
        Kind::Invalid,
        "adaptive plan exceeds milestone capacity",
    )?;
    plan.outputs = previous.into_iter().collect();
    plan.revision = revision;
    plan.parent_digest = Some(state.plan.digest()?);
    validate_serial(&plan)?;
    Ok(plan)
}

pub fn validate_serial(plan: &Plan) -> Result<()> {
    require(
        plan.max_parallel == 1 && plan.nodes.len() >= 3,
        Kind::Invalid,
        "adaptive workflow must be serial and multi-specialist",
    )?;
    let profiles: BTreeSet<_> = plan.nodes.iter().map(|n| &n.profile).collect();
    require(
        profiles.len() >= 2
            && plan
                .nodes
                .last()
                .is_some_and(|n| n.profile == "melissa.code.reviewer")
            && plan
                .nodes
                .iter()
                .any(|n| n.profile == "melissa.code.test-engineer"),
        Kind::Invalid,
        "adaptive plan needs testing and independent final review",
    )?;

    let last_test = plan
        .nodes
        .iter()
        .rposition(|n| n.profile == "melissa.code.test-engineer")
        .expect("checked");
    require(
        last_test < plan.nodes.len() - 1,
        Kind::Invalid,
        "testing must precede final review",
    )?;
    for (index, node) in plan.nodes.iter().enumerate() {
        let mutates = node
            .claims
            .iter()
            .any(|c| c.access == crate::plan::Access::Write)
            || node.effects.iter().any(|e| {
                matches!(
                    e.as_str(),
                    "workspace.apply_patch_scoped"
                        | "workspace.create_worktree_scoped"
                        | "git.write_scoped"
                )
            });
        require(
            !mutates || index < last_test,
            Kind::Authority,
            "all writes must precede final testing and review",
        )?;
        if index == last_test || index == plan.nodes.len() - 1 {
            require(
                !mutates,
                Kind::Authority,
                "independent verification roles cannot mutate source",
            )?;
        }
        require(
            node.bindings.is_empty(),
            Kind::Invalid,
            "adaptive handoffs use evidence references, not raw predecessor content",
        )?;
    }
    let mut previous = None;
    for node in &plan.nodes {
        require(
            node.after == previous.iter().cloned().collect(),
            Kind::Invalid,
            "adaptive milestones must form one serial chain",
        )?;
        previous = Some(node.id.clone());
    }
    require(
        plan.outputs == previous.into_iter().collect(),
        Kind::Invalid,
        "adaptive output must be the final reviewer",
    )
}
