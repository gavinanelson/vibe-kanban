use std::{
    collections::HashSet,
    future::Future,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use api_types::{Issue, PullRequestStatus, UpdateIssueRequest, UpsertPullRequestRequest};
use axum::{
    Extension, Json, Router,
    extract::State,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use db::models::{
    execution_process::{ExecutionProcessRunReason, ExecutionProcessStatus},
    merge::{Merge, MergeStatus, PrMerge},
    pull_request::PullRequest,
    session::{CreateSession, Session},
    workspace::{Workspace, WorkspaceError},
    workspace_repo::WorkspaceRepo,
};
use deployment::Deployment;
use executors::{
    actions::{
        ExecutorAction, ExecutorActionType,
        coding_agent_initial::CodingAgentInitialRequest,
        review::{RepoReviewContext as ExecutorRepoReviewContext, ReviewRequest as ReviewAction},
    },
    executors::{BaseCodingAgent, build_review_prompt},
    model_selector::PermissionPolicy,
    profile::ExecutorConfig,
};
use git_host::{GitHostProvider, GitHostService, PullRequestDetail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use services::services::{container::ContainerService, remote_client::RemoteClient, remote_sync};
use sqlx::FromRow;
use ts_rs::TS;
use utils::{log_msg::LogMsg, response::ApiResponse};
use uuid::Uuid;

use crate::{DeploymentImpl, error::ApiError, middleware::load_workspace_middleware};

const DEFAULT_CODEX_MODEL: &str = "gpt-5.5";
const DEFAULT_CODEX_REASONING: &str = "medium";
const REVIEW_PROMPT: &str = r#"You are the app-owned Implication autopilot reviewer.

This is an unattended review session. Do not implement changes. Do not ask the operator to do routine follow-up. Your job is to decide whether the current workspace/PR/head is safe for the app to move toward merge.

Review discipline:
- Re-read the linked GitHub issue and treat its acceptance criteria, validation/test-plan sections, and labels as the contract.
- Inspect the current diff/PR scope, not stale prior review output.
- Check whether the implementation includes credible validation evidence for the changed behavior.
- Look for blockers only: correctness regressions, missing acceptance criteria, unsafe merge/PR state, unresolved conflicts, failing or missing required checks, security/data-loss risk, or hygiene failures that would make the PR unsafe to merge.
- Do not request changes for style preferences, speculative improvements, or unrelated cleanup. Put those under non-blocking notes.
- If PR/check/mergeability state is unavailable from your tools, report that as a blocker only when it prevents a safe merge decision.

Required final format:

Decision: pass
Blockers:
- None
Validation evidence:
- ...
Non-blocking notes:
- ...
Recommended next action:
- Merge when app-visible PR checks and mergeability are green.

or:

Decision: request changes
Blockers:
- ...
Validation evidence:
- ...
Non-blocking notes:
- ...
Recommended next action:
- Start a bounded review-fix session for the blockers above.

The first line must be exactly one of `Decision: pass` or `Decision: request changes`."#;
const REVIEW_FIX_PROMPT: &str = r#"You are the Implication autopilot fix worker continuing an existing task.

This is an unattended orchestration session. Do not ask the operator for routine next steps. Only stop early for a true external blocker: missing required auth, permissions, secrets, or tooling after checking documented fallbacks.

Scope:
- Fix only the blocking issues from the latest auto-review excerpt below.
- Preserve unrelated dirty work and avoid unrelated refactors.
- Re-read the linked GitHub issue before editing. Treat acceptance criteria and any Validation/Test Plan/Testing section as mandatory.
- Work from the current workspace state. Do not restart the task from scratch unless the review explicitly requires that.

Execution protocol:
1. Identify each blocker you are addressing.
2. Inspect the current diff and relevant code before editing.
3. Make the smallest coherent fix that satisfies the issue contract.
4. Run focused validation that directly proves the fix. Do not run broad guarded validation unless explicitly allowed by repo policy.
5. If validation fails, fix and rerun until the focused proof is green or a true blocker remains.
6. Do not merge the PR and do not claim it is merged. The app owns re-review and merge.

Final response must include:
- Files changed
- Blockers fixed
- Validation run and result
- Remaining blockers, or `None`
- Whether another auto-review is needed

Keep the response concise and factual. The app will start a new auto-review after this session completes."#;

static ADVANCE_LOCKS: OnceLock<Mutex<HashSet<Uuid>>> = OnceLock::new();

fn default_batch_max_active() -> usize {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum AutopilotDecision {
    Missing,
    Running,
    Pass,
    RequestChanges,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum AutopilotNextAction {
    NoWorkspace,
    WaitForImplementation,
    StartAutoReview,
    WaitForAutoReview,
    StartReviewFix,
    WaitForReviewFix,
    ReadyForMerge,
    MergeWait,
    Done,
    InvestigateFailure,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum AutopilotWorkflowState {
    Queued,
    BlockedByDependencies,
    ImplementationRunning,
    ReviewRunning,
    ReviewPassed,
    ReviewRequestedChanges,
    ReviewFixRunning,
    MergeWaiting,
    Done,
    Blocked,
    ReadyToAdvance,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum AutopilotTokenSafetyState {
    Idle,
    Guarded,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum AutopilotPrChecksState {
    Unknown,
    NoChecks,
    Pending,
    Passing,
    Failing,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct AutopilotPrStatus {
    pub number: i64,
    pub url: String,
    pub state: String,
    pub is_draft: bool,
    pub head_sha: Option<String>,
    pub base_branch: Option<String>,
    pub mergeable: Option<String>,
    pub merge_state_status: Option<String>,
    pub merge_commit_sha: Option<String>,
    pub checks_state: AutopilotPrChecksState,
    pub checks_summary: String,
    pub merge_blocker: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct AutopilotProcessSummary {
    pub id: Uuid,
    pub session_id: Uuid,
    pub session_name: Option<String>,
    pub status: ExecutionProcessStatus,
    pub run_reason: ExecutionProcessRunReason,
    pub exit_code: Option<i64>,
    pub started_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ImplicationAutopilotStatus {
    pub workspace_id: Uuid,
    pub workspace_name: Option<String>,
    pub implementation_state: String,
    pub auto_review_state: AutopilotDecision,
    pub latest_review_decision: AutopilotDecision,
    pub latest_review_excerpt: Option<String>,
    pub review_fix_state: String,
    pub pr_merge_state: String,
    pub next_action: AutopilotNextAction,
    pub blocker: Option<String>,
    pub implementation_process: Option<AutopilotProcessSummary>,
    pub auto_review_process: Option<AutopilotProcessSummary>,
    pub review_fix_process: Option<AutopilotProcessSummary>,
    pub default_model: String,
    pub default_reasoning: String,
    pub daemonized: bool,
    pub workflow_state: AutopilotWorkflowState,
    pub workflow_state_reason: String,
    pub duplicate_prevention_key: String,
    pub token_safety_state: AutopilotTokenSafetyState,
    pub token_safety_note: String,
    pub pr_status: Option<AutopilotPrStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ImplicationAutopilotAdvanceResponse {
    pub action_taken: AutopilotAdvanceAction,
    pub status: ImplicationAutopilotStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum AutopilotAdvanceAction {
    Noop,
    PromotedToReview,
    StartedAutoReview,
    StartedReviewFix,
    MergedPullRequest,
    MergeHandoff,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AutopilotMergePlan {
    AttemptPrMerge,
    ReconcileDone,
    Blocked(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum BatchCardQueueState {
    Queued,
    BlockedByDependencies,
    Runnable,
    Active,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct BatchAdvanceCard {
    pub issue_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub state: BatchCardQueueState,
    pub blockers: Vec<Uuid>,
    pub action: Option<AutopilotNextAction>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct BatchAdvancePlan {
    pub max_active: usize,
    pub active_count: usize,
    pub cards: Vec<BatchAdvanceCard>,
}

#[derive(Debug, Clone)]
struct BatchCandidate {
    issue_id: Uuid,
    workspace_id: Option<Uuid>,
    done: bool,
    active: bool,
    next_action: Option<AutopilotNextAction>,
    blockers: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct BatchAdvancePlanRequest {
    #[serde(default = "default_batch_max_active")]
    pub max_active: usize,
    pub cards: Vec<BatchAdvancePlanCandidate>,
    #[serde(default)]
    pub relationships: Vec<BatchAdvanceRelationship>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct BatchAdvanceRelationship {
    pub issue_id: Uuid,
    pub blocking_issue_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct BatchAdvancePlanCandidate {
    pub issue_id: Uuid,
    pub workspace_id: Option<Uuid>,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub active: bool,
    pub next_action: Option<AutopilotNextAction>,
    #[serde(default)]
    pub blockers: Vec<Uuid>,
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct StartAutopilotReviewRequest {
    #[serde(default)]
    pub rerun: bool,
    pub github_repo_full_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct StartAutopilotReviewFixRequest {
    pub github_repo_full_name: Option<String>,
}

#[derive(Debug, FromRow, Clone)]
struct ProcessRow {
    id: Uuid,
    session_id: Uuid,
    session_name: Option<String>,
    status: ExecutionProcessStatus,
    run_reason: ExecutionProcessRunReason,
    exit_code: Option<i64>,
    started_at: String,
    completed_at: Option<String>,
}

pub fn router(deployment: &DeploymentImpl) -> Router<DeploymentImpl> {
    let workspace_routes = Router::new()
        .route(
            "/workspaces/{id}/implication-autopilot/status",
            get(get_status),
        )
        .route(
            "/workspaces/{id}/implication-autopilot/advance",
            post(advance_workspace),
        )
        .route(
            "/workspaces/{id}/implication-autopilot/review",
            post(start_auto_review),
        )
        .route(
            "/workspaces/{id}/implication-autopilot/review-fix",
            post(start_review_fix),
        )
        .layer(axum::middleware::from_fn_with_state(
            deployment.clone(),
            load_workspace_middleware,
        ));

    Router::new()
        .route("/implication-autopilot/batch/plan", post(plan_batch))
        .merge(workspace_routes)
}

#[axum::debug_handler]
async fn get_status(
    Extension(workspace): Extension<Workspace>,
    State(deployment): State<DeploymentImpl>,
) -> Result<Json<ApiResponse<ImplicationAutopilotStatus>>, ApiError> {
    let status = build_status(&deployment, &workspace).await?;
    Ok(Json(ApiResponse::success(status)))
}

#[axum::debug_handler]
async fn plan_batch(
    Json(payload): Json<BatchAdvancePlanRequest>,
) -> Result<Json<ApiResponse<BatchAdvancePlan>>, ApiError> {
    let candidates = payload
        .cards
        .into_iter()
        .map(|card| BatchCandidate {
            issue_id: card.issue_id,
            workspace_id: card.workspace_id,
            done: card.done,
            active: card.active,
            next_action: card.next_action,
            blockers: card.blockers,
        })
        .collect::<Vec<_>>();
    Ok(Json(ApiResponse::success(plan_batch_advance(
        &candidates,
        &payload.relationships,
        payload.max_active,
    ))))
}

#[axum::debug_handler]
async fn advance_workspace(
    Extension(workspace): Extension<Workspace>,
    State(deployment): State<DeploymentImpl>,
) -> Result<Json<ApiResponse<ImplicationAutopilotAdvanceResponse>>, ApiError> {
    ensure_implication_autopilot_allowed(&deployment, &workspace).await?;

    let (action_taken, merge_blocker) = advance_workspace_once(&deployment, &workspace).await?;
    let status_workspace = Workspace::find_by_id(&deployment.db().pool, workspace.id)
        .await?
        .unwrap_or(workspace);
    let mut status = build_status(&deployment, &status_workspace).await?;
    if let Some(blocker) = merge_blocker {
        status.blocker = Some(blocker.clone());
        status.next_action = AutopilotNextAction::InvestigateFailure;
        status.workflow_state = AutopilotWorkflowState::Blocked;
        status.workflow_state_reason = blocker;
        status.token_safety_state = AutopilotTokenSafetyState::Blocked;
        status.token_safety_note =
            "Merge/done reconciliation is blocked before another Codex session can start."
                .to_string();
    }
    Ok(Json(ApiResponse::success(
        ImplicationAutopilotAdvanceResponse {
            action_taken,
            status,
        },
    )))
}

#[axum::debug_handler]
async fn start_auto_review(
    Extension(workspace): Extension<Workspace>,
    State(deployment): State<DeploymentImpl>,
    Json(payload): Json<StartAutopilotReviewRequest>,
) -> Result<Json<ApiResponse<AutopilotProcessSummary>>, ApiError> {
    ensure_implication_autopilot_allowed(&deployment, &workspace).await?;
    let process = start_auto_review_for_workspace(&deployment, &workspace, payload.rerun).await?;
    Ok(Json(ApiResponse::success(process)))
}

#[axum::debug_handler]
async fn start_review_fix(
    Extension(workspace): Extension<Workspace>,
    State(deployment): State<DeploymentImpl>,
    Json(_payload): Json<StartAutopilotReviewFixRequest>,
) -> Result<Json<ApiResponse<AutopilotProcessSummary>>, ApiError> {
    ensure_implication_autopilot_allowed(&deployment, &workspace).await?;
    let process = start_review_fix_for_workspace(&deployment, &workspace).await?;
    Ok(Json(ApiResponse::success(process)))
}

async fn start_auto_review_for_workspace(
    deployment: &DeploymentImpl,
    workspace: &Workspace,
    rerun: bool,
) -> Result<AutopilotProcessSummary, ApiError> {
    if db::models::execution_process::ExecutionProcess::has_running_non_dev_server_processes_for_workspace(
        &deployment.db().pool,
        workspace.id,
    )
    .await?
    {
        return Err(ApiError::Workspace(WorkspaceError::ValidationError(
            "Workspace already has a running agent process".to_string(),
        )));
    }

    let rows = list_processes(&deployment.db().pool, workspace.id).await?;
    let implementation_process = rows.iter().find(|row| is_implementation_process(row));
    let review_processes: Vec<&ProcessRow> = rows
        .iter()
        .filter(|row| is_auto_review_session(row.session_name.as_deref()))
        .collect();
    let review_fix_process = rows
        .iter()
        .find(|row| is_review_fix_session(row.session_name.as_deref()));
    let (latest_review_decision, _, latest_review_process) =
        decide_from_review_attempts(deployment, &review_processes).await;
    let pr_status = latest_pr_status(deployment, workspace).await;
    let pr_merge_state = infer_pr_merge_state(
        deployment,
        workspace,
        &latest_review_decision,
        pr_status.as_ref(),
    )
    .await?;

    if let Err(message) = auto_review_start_gate(
        workspace,
        implementation_process,
        &latest_review_decision,
        latest_review_process,
        review_fix_process,
        &pr_merge_state,
        pr_status.as_ref(),
        rerun,
    ) {
        return Err(ApiError::Workspace(WorkspaceError::ValidationError(
            message,
        )));
    }

    let session = Session::create(
        &deployment.db().pool,
        &CreateSession {
            executor: Some("CODEX".to_string()),
            name: Some(auto_review_session_name(rerun)),
        },
        Uuid::new_v4(),
        workspace.id,
    )
    .await?;

    let container_ref = deployment
        .container()
        .ensure_container_exists(workspace)
        .await?;
    let context =
        build_autopilot_review_context(deployment, workspace, container_ref.as_str()).await?;
    let executor_config = default_codex_config();
    let prompt = build_review_prompt(context.as_deref(), Some(REVIEW_PROMPT));
    let action = ExecutorAction::new(
        ExecutorActionType::ReviewRequest(ReviewAction {
            executor_config,
            context,
            prompt,
            session_id: None,
            working_dir: session.agent_working_dir.clone(),
        }),
        None,
    );
    let process = deployment
        .container()
        .start_execution(
            workspace,
            &session,
            &action,
            &ExecutionProcessRunReason::CodingAgent,
        )
        .await?;

    Ok(process_summary_from_parts(
        process.id,
        process.session_id,
        session.name,
        process.status,
        process.run_reason,
        process.exit_code,
        process.started_at.to_rfc3339(),
        process
            .completed_at
            .map(|dt: DateTime<Utc>| dt.to_rfc3339()),
    ))
}

async fn start_review_fix_for_workspace(
    deployment: &DeploymentImpl,
    workspace: &Workspace,
) -> Result<AutopilotProcessSummary, ApiError> {
    if db::models::execution_process::ExecutionProcess::has_running_non_dev_server_processes_for_workspace(
        &deployment.db().pool,
        workspace.id,
    )
    .await?
    {
        return Err(ApiError::Workspace(WorkspaceError::ValidationError(
            "Workspace already has a running agent process".to_string(),
        )));
    }

    let rows = list_processes(&deployment.db().pool, workspace.id).await?;
    let implementation_process = rows.iter().find(|row| is_implementation_process(row));
    let review_processes: Vec<&ProcessRow> = rows
        .iter()
        .filter(|row| is_auto_review_session(row.session_name.as_deref()))
        .collect();
    let review_fix_process = rows
        .iter()
        .find(|row| is_review_fix_session(row.session_name.as_deref()));
    let (latest_review_decision, latest_review_excerpt, latest_review_process) =
        decide_from_review_attempts(deployment, &review_processes).await;

    if let Err(message) = review_fix_start_gate(
        workspace,
        implementation_process,
        &latest_review_decision,
        latest_review_process,
        review_fix_process,
    ) {
        return Err(ApiError::Workspace(WorkspaceError::ValidationError(
            message,
        )));
    }

    let session = Session::create(
        &deployment.db().pool,
        &CreateSession {
            executor: Some("CODEX".to_string()),
            name: Some(review_fix_session_name()),
        },
        Uuid::new_v4(),
        workspace.id,
    )
    .await?;

    deployment
        .container()
        .ensure_container_exists(workspace)
        .await?;

    let prompt = match latest_review_excerpt {
        Some(excerpt) => format!("{REVIEW_FIX_PROMPT}\n\nLatest auto-review excerpt:\n{excerpt}"),
        None => REVIEW_FIX_PROMPT.to_string(),
    };
    let working_dir = session
        .agent_working_dir
        .as_ref()
        .filter(|dir| !dir.is_empty())
        .cloned();
    let action = ExecutorAction::new(
        ExecutorActionType::CodingAgentInitialRequest(CodingAgentInitialRequest {
            prompt,
            executor_config: default_codex_config(),
            working_dir,
        }),
        None,
    );
    let process = deployment
        .container()
        .start_execution(
            workspace,
            &session,
            &action,
            &ExecutionProcessRunReason::CodingAgent,
        )
        .await?;

    Ok(process_summary_from_parts(
        process.id,
        process.session_id,
        session.name,
        process.status,
        process.run_reason,
        process.exit_code,
        process.started_at.to_rfc3339(),
        process
            .completed_at
            .map(|dt: DateTime<Utc>| dt.to_rfc3339()),
    ))
}

async fn advance_workspace_once(
    deployment: &DeploymentImpl,
    workspace: &Workspace,
) -> Result<(AutopilotAdvanceAction, Option<String>), ApiError> {
    let Some(_advance_guard) = WorkspaceAdvanceGuard::try_acquire(workspace.id) else {
        return Ok((AutopilotAdvanceAction::Noop, None));
    };

    let client = deployment.remote_client()?;
    let remote_workspace = client.get_workspace_by_local_id(workspace.id).await?;
    let issue = match remote_workspace.issue_id {
        Some(issue_id) => Some(client.get_issue(issue_id).await?),
        None => None,
    };

    let status = build_status(deployment, workspace).await?;
    match status.next_action {
        AutopilotNextAction::StartAutoReview => {
            let rerun = status.next_action == AutopilotNextAction::StartAutoReview
                && status.latest_review_decision == AutopilotDecision::RequestChanges;
            start_auto_review_for_workspace(deployment, workspace, rerun).await?;
            if let Some(issue) = issue.as_ref() {
                promote_issue_to_in_review(&client, issue).await?;
            }
            Ok((AutopilotAdvanceAction::StartedAutoReview, None))
        }
        AutopilotNextAction::StartReviewFix => {
            start_review_fix_for_workspace(deployment, workspace).await?;
            Ok((AutopilotAdvanceAction::StartedReviewFix, None))
        }
        AutopilotNextAction::ReadyForMerge => {
            advance_merge_done_once(
                deployment,
                workspace,
                issue.as_ref(),
                &status.pr_merge_state,
            )
            .await
        }
        AutopilotNextAction::WaitForImplementation => Ok((AutopilotAdvanceAction::Noop, None)),
        AutopilotNextAction::NoWorkspace | AutopilotNextAction::InvestigateFailure => {
            Ok((AutopilotAdvanceAction::Blocked, status.blocker))
        }
        AutopilotNextAction::WaitForAutoReview
        | AutopilotNextAction::WaitForReviewFix
        | AutopilotNextAction::MergeWait
        | AutopilotNextAction::Done => Ok((AutopilotAdvanceAction::Noop, None)),
    }
}

struct WorkspaceAdvanceGuard {
    workspace_id: Uuid,
}

impl WorkspaceAdvanceGuard {
    fn try_acquire(workspace_id: Uuid) -> Option<Self> {
        let locks = ADVANCE_LOCKS.get_or_init(|| Mutex::new(HashSet::new()));
        let mut active = locks.lock().ok()?;
        if !active.insert(workspace_id) {
            return None;
        }
        Some(Self { workspace_id })
    }
}

impl Drop for WorkspaceAdvanceGuard {
    fn drop(&mut self) {
        if let Some(locks) = ADVANCE_LOCKS.get()
            && let Ok(mut active) = locks.lock()
        {
            active.remove(&self.workspace_id);
        }
    }
}

async fn advance_merge_done_once(
    deployment: &DeploymentImpl,
    workspace: &Workspace,
    issue: Option<&Issue>,
    pr_merge_state: &str,
) -> Result<(AutopilotAdvanceAction, Option<String>), ApiError> {
    match merge_plan_from_state(pr_merge_state) {
        AutopilotMergePlan::ReconcileDone => {
            reconcile_workspace_done(deployment, workspace, issue).await?;
            Ok((AutopilotAdvanceAction::Noop, None))
        }
        AutopilotMergePlan::Blocked(blocker) => {
            Ok((AutopilotAdvanceAction::Blocked, Some(blocker)))
        }
        AutopilotMergePlan::AttemptPrMerge => {
            match merge_open_pull_requests(deployment, workspace).await {
                Ok(()) => {
                    reconcile_workspace_done(deployment, workspace, issue).await?;
                    Ok((AutopilotAdvanceAction::Noop, None))
                }
                Err(blocker) => Ok((AutopilotAdvanceAction::Blocked, Some(blocker))),
            }
        }
    }
}

fn merge_plan_from_state(pr_merge_state: &str) -> AutopilotMergePlan {
    match pr_merge_state {
        "merged_pending_done" => AutopilotMergePlan::ReconcileDone,
        "pr_open_pending_merge" => AutopilotMergePlan::AttemptPrMerge,
        "blocked_by_dirty_worktree" => AutopilotMergePlan::Blocked(
            "Merge is blocked by dirty/conflicting workspace changes.".to_string(),
        ),
        "blocked_by_pr_requirements" => AutopilotMergePlan::Blocked(
            "PR merge is blocked by GitHub checks, reviews, conflicts, or mergeability."
                .to_string(),
        ),
        "blocked_by_draft_pr" => AutopilotMergePlan::Blocked("PR is still a draft.".to_string()),
        "blocked_by_failing_checks" => {
            AutopilotMergePlan::Blocked("PR has failing checks.".to_string())
        }
        "blocked_by_pr_mergeability" => AutopilotMergePlan::Blocked(
            "PR is not currently mergeable according to GitHub.".to_string(),
        ),
        "waiting_for_checks" => {
            AutopilotMergePlan::Blocked("PR checks are not green yet.".to_string())
        }
        "review_passed_merge_status_unknown" => AutopilotMergePlan::AttemptPrMerge,
        "no_pr_merge_found" => AutopilotMergePlan::Blocked(
            "Review passed, but no open or merged pull request is linked to this workspace."
                .to_string(),
        ),
        _ => AutopilotMergePlan::Blocked(
            "Review passed, but merge state is not specific enough for the app to advance safely."
                .to_string(),
        ),
    }
}

async fn merge_open_pull_requests(
    deployment: &DeploymentImpl,
    workspace: &Workspace,
) -> Result<(), String> {
    let merges = Merge::find_by_workspace_id(&deployment.db().pool, workspace.id)
        .await
        .map_err(|err| format!("Failed to inspect linked pull requests: {err}"))?;
    let linked_prs = merges
        .iter()
        .filter_map(|merge| match merge {
            Merge::Pr(pr) => Some(pr.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();

    if linked_prs.is_empty() {
        return Err(linked_pr_merge_completion_blocker(&[]).unwrap_or_else(|| {
            "No linked pull requests were found for this workspace; create or link a PR and rerun app advance."
                .to_string()
        }));
    }

    let mut final_statuses = Vec::with_capacity(linked_prs.len());
    let mut blockers = Vec::new();
    for linked_pr in &linked_prs {
        if matches!(linked_pr.pr_info.status, MergeStatus::Merged) {
            final_statuses.push(MergeStatus::Merged);
            continue;
        }

        let git_host = match GitHostService::from_url(&linked_pr.pr_info.url) {
            Ok(git_host) => git_host,
            Err(err) => {
                blockers.push(format!(
                    "Cannot identify pull request provider for PR #{} ({}): {err}",
                    linked_pr.pr_info.number, linked_pr.pr_info.url
                ));
                final_statuses.push(linked_pr.pr_info.status.clone());
                continue;
            }
        };
        let current_pr_info = match git_host.get_pr_status(&linked_pr.pr_info.url).await {
            Ok(pr_info) => pr_info,
            Err(err) => {
                blockers.push(format!(
                    "Failed to refresh PR #{} ({}) merge status from GitHub: {err}",
                    linked_pr.pr_info.number, linked_pr.pr_info.url
                ));
                final_statuses.push(linked_pr.pr_info.status.clone());
                continue;
            }
        };
        if !matches!(
            current_pr_info.status,
            MergeStatus::Open | MergeStatus::Merged
        ) {
            let status = current_pr_info.status.clone();
            if let Err(err) =
                persist_pr_status(deployment, workspace, linked_pr, current_pr_info).await
            {
                blockers.push(format!(
                    "PR #{} ({}) refreshed as {}, but status persistence failed: {err}",
                    linked_pr.pr_info.number,
                    linked_pr.pr_info.url,
                    merge_status_label(&status)
                ));
            }
            final_statuses.push(status.clone());
            blockers.push(format!(
                "Cannot complete autopilot merge: PR #{} ({}) is {} after refresh.",
                linked_pr.pr_info.number,
                linked_pr.pr_info.url,
                merge_status_label(&status)
            ));
            continue;
        }

        let pr_info = if matches!(current_pr_info.status, MergeStatus::Merged) {
            current_pr_info
        } else {
            let pr_info = match git_host.merge_pr(&linked_pr.pr_info.url).await {
                Ok(pr_info) => pr_info,
                Err(err) => {
                    let persist_result =
                        persist_pr_status(deployment, workspace, linked_pr, current_pr_info).await;
                    let persist_note = persist_result
                        .err()
                        .map(|persist_err| {
                            format!(" Refreshed PR status persistence also failed: {persist_err}.")
                        })
                        .unwrap_or_default();
                    blockers.push(format!(
                        "PR #{} ({}) merge is blocked by GitHub requirements: {err}.{persist_note}",
                        linked_pr.pr_info.number, linked_pr.pr_info.url
                    ));
                    final_statuses.push(MergeStatus::Open);
                    continue;
                }
            };

            if !matches!(pr_info.status, MergeStatus::Merged) {
                let status = pr_info.status.clone();
                if let Err(err) = persist_pr_status(deployment, workspace, linked_pr, pr_info).await
                {
                    blockers.push(format!(
                        "PR #{} ({}) status persistence failed after merge attempt: {err}",
                        linked_pr.pr_info.number, linked_pr.pr_info.url
                    ));
                }
                final_statuses.push(status.clone());
                blockers.push(format!(
                    "GitHub accepted the merge command for PR #{} ({}), but it is {} instead of merged.",
                    linked_pr.pr_info.number,
                    linked_pr.pr_info.url,
                    merge_status_label(&status)
                ));
                continue;
            }
            pr_info
        };

        if let Err(err) = persist_pr_status(deployment, workspace, linked_pr, pr_info).await {
            blockers.push(format!(
                "PR #{} ({}) merged, but local/remote PR status update failed: {err}",
                linked_pr.pr_info.number, linked_pr.pr_info.url
            ));
        }
        final_statuses.push(MergeStatus::Merged);
    }

    if blockers.is_empty() {
        linked_pr_merge_completion_blocker(&final_statuses).map_or(Ok(()), Err)
    } else {
        Err(blockers.join(" "))
    }
}

fn linked_pr_merge_completion_blocker(statuses: &[MergeStatus]) -> Option<String> {
    if statuses.is_empty() {
        return Some(
            "No linked pull requests were found for this workspace; create or link a PR and rerun app advance."
                .to_string(),
        );
    }

    let open_count = statuses
        .iter()
        .filter(|status| matches!(status, MergeStatus::Open))
        .count();
    if open_count > 0 {
        return Some(format!(
            "Not all linked pull requests are merged yet: {open_count} open PR{} remain{} after merge attempts.",
            if open_count == 1 { "" } else { "s" },
            if open_count == 1 { "s" } else { "" }
        ));
    }

    let closed_count = statuses
        .iter()
        .filter(|status| matches!(status, MergeStatus::Closed))
        .count();
    if closed_count > 0 {
        return Some(format!(
            "Cannot complete autopilot merge: {closed_count} linked PR{} {} closed.",
            if closed_count == 1 { "" } else { "s" },
            if closed_count == 1 { "is" } else { "are" }
        ));
    }

    let unknown_count = statuses
        .iter()
        .filter(|status| matches!(status, MergeStatus::Unknown))
        .count();
    if unknown_count > 0 {
        return Some(format!(
            "Cannot complete autopilot merge: {unknown_count} linked PR{} ha{} unknown status.",
            if unknown_count == 1 { "" } else { "s" },
            if unknown_count == 1 { "s" } else { "ve" }
        ));
    }

    None
}

fn merge_status_label(status: &MergeStatus) -> &'static str {
    match status {
        MergeStatus::Open => "open",
        MergeStatus::Merged => "merged",
        MergeStatus::Closed => "closed",
        MergeStatus::Unknown => "unknown",
    }
}

async fn persist_pr_status(
    deployment: &DeploymentImpl,
    workspace: &Workspace,
    linked_pr: &PrMerge,
    pr_detail: PullRequestDetail,
) -> Result<(), String> {
    let status = pr_detail.status.clone();
    let merged_at = pr_detail.merged_at;
    let merge_commit_sha = pr_detail.merge_commit_sha.clone();
    let url = pr_detail.url.clone();

    PullRequest::update_status(
        &deployment.db().pool,
        &url,
        &status,
        merged_at,
        merge_commit_sha.clone(),
    )
    .await
    .map_err(|err| format!("Local PR status update failed: {err}"))?;

    if let Ok(client) = deployment.remote_client() {
        let request = UpsertPullRequestRequest {
            url,
            number: pr_detail.number as i32,
            status: pull_request_status_from_merge_status(&status),
            merged_at,
            merge_commit_sha,
            target_branch_name: linked_pr.target_branch_name.clone(),
            local_workspace_id: workspace.id,
        };
        tokio::spawn(async move {
            remote_sync::sync_pr_to_remote(&client, request).await;
        });
    }

    Ok(())
}

fn pull_request_status_from_merge_status(status: &MergeStatus) -> PullRequestStatus {
    match status {
        MergeStatus::Merged => PullRequestStatus::Merged,
        MergeStatus::Closed => PullRequestStatus::Closed,
        MergeStatus::Open | MergeStatus::Unknown => PullRequestStatus::Open,
    }
}

async fn reconcile_workspace_done(
    deployment: &DeploymentImpl,
    workspace: &Workspace,
    issue: Option<&Issue>,
) -> Result<(), ApiError> {
    if let Some(issue) = issue {
        promote_issue_to_done(&deployment.remote_client()?, issue).await?;
    }

    if !workspace.pinned {
        deployment
            .container()
            .archive_workspace(workspace.id)
            .await?;
    }

    Ok(())
}

async fn build_autopilot_review_context(
    deployment: &DeploymentImpl,
    workspace: &Workspace,
    container_path: &str,
) -> Result<Option<Vec<ExecutorRepoReviewContext>>, ApiError> {
    let repos = WorkspaceRepo::find_repos_with_target_branch_for_workspace(
        &deployment.db().pool,
        workspace.id,
    )
    .await?;
    let workspace_path = PathBuf::from(container_path);

    let mut contexts = Vec::new();
    for repo in repos {
        let worktree_path = workspace_path.join(&repo.repo.name);
        if let Ok(base_commit) =
            deployment
                .git()
                .get_fork_point(&worktree_path, &repo.target_branch, &workspace.branch)
        {
            contexts.push(ExecutorRepoReviewContext {
                repo_id: repo.repo.id,
                repo_name: repo.repo.display_name,
                base_commit,
            });
        }
    }

    Ok((!contexts.is_empty()).then_some(contexts))
}

async fn ensure_implication_autopilot_allowed(
    deployment: &DeploymentImpl,
    workspace: &Workspace,
) -> Result<(), ApiError> {
    let client = deployment.remote_client()?;
    let remote_workspace = client.get_workspace_by_local_id(workspace.id).await?;
    let Some(issue_id) = remote_workspace.issue_id else {
        return Err(ApiError::Workspace(WorkspaceError::ValidationError(
            "Implication autopilot requires a linked board issue.".to_string(),
        )));
    };

    let issue = client.get_issue(issue_id).await?;
    if is_implication_issue_metadata(&issue.extension_metadata) {
        return Ok(());
    }

    Err(ApiError::Workspace(WorkspaceError::ValidationError(
        "Implication autopilot is only enabled for gavinanelson/implication issues.".to_string(),
    )))
}

fn is_implication_issue_metadata(metadata: &Value) -> bool {
    let Some(repo) = metadata
        .get("github_link")
        .and_then(|link| link.get("repo_full_name"))
        .and_then(Value::as_str)
    else {
        return false;
    };

    repo.trim().eq_ignore_ascii_case("gavinanelson/implication")
}

fn auto_review_session_name(rerun: bool) -> String {
    format!(
        "Auto review{} - Codex ({})",
        if rerun { " rerun" } else { "" },
        DEFAULT_CODEX_REASONING
    )
}

fn review_fix_session_name() -> String {
    format!("Review fix - Codex ({DEFAULT_CODEX_REASONING})")
}

fn default_codex_config() -> ExecutorConfig {
    ExecutorConfig {
        executor: BaseCodingAgent::Codex,
        variant: None,
        model_id: Some(DEFAULT_CODEX_MODEL.to_string()),
        agent_id: None,
        reasoning_id: Some(DEFAULT_CODEX_REASONING.to_string()),
        permission_policy: Some(PermissionPolicy::Auto),
    }
}

async fn promote_issue_to_in_review(
    client: &RemoteClient,
    issue: &Issue,
) -> Result<bool, ApiError> {
    let statuses = client.list_project_statuses(issue.project_id).await?;
    let Some(in_review) = statuses
        .project_statuses
        .iter()
        .find(|status| status.name.trim().eq_ignore_ascii_case("in review"))
    else {
        return Ok(false);
    };

    if issue.status_id == in_review.id {
        return Ok(false);
    }

    client
        .update_issue(
            issue.id,
            &UpdateIssueRequest {
                status_id: Some(in_review.id),
                title: None,
                description: None,
                priority: None,
                start_date: None,
                target_date: None,
                completed_at: None,
                sort_order: None,
                parent_issue_id: None,
                parent_issue_sort_order: None,
                extension_metadata: None,
            },
        )
        .await?;
    Ok(true)
}

async fn promote_issue_to_done(client: &RemoteClient, issue: &Issue) -> Result<bool, ApiError> {
    let statuses = client.list_project_statuses(issue.project_id).await?;
    let Some(done) = statuses
        .project_statuses
        .iter()
        .find(|status| status.name.trim().eq_ignore_ascii_case("done"))
    else {
        return Ok(false);
    };

    if issue.status_id == done.id && issue.completed_at.is_some() {
        return Ok(false);
    }

    client
        .update_issue(
            issue.id,
            &UpdateIssueRequest {
                status_id: Some(done.id),
                title: None,
                description: None,
                priority: None,
                start_date: None,
                target_date: None,
                completed_at: Some(Some(Utc::now())),
                sort_order: None,
                parent_issue_id: None,
                parent_issue_sort_order: None,
                extension_metadata: None,
            },
        )
        .await?;
    Ok(true)
}

async fn build_status(
    deployment: &DeploymentImpl,
    workspace: &Workspace,
) -> Result<ImplicationAutopilotStatus, ApiError> {
    let rows = list_processes(&deployment.db().pool, workspace.id).await?;
    let implementation_process = rows.iter().find(|row| is_implementation_process(row));
    let review_processes: Vec<&ProcessRow> = rows
        .iter()
        .filter(|row| is_auto_review_session(row.session_name.as_deref()))
        .collect();
    let review_fix_process = rows
        .iter()
        .find(|row| is_review_fix_session(row.session_name.as_deref()));

    let (latest_review_decision, latest_review_excerpt, auto_review_process) =
        decide_from_review_attempts(deployment, &review_processes).await;
    let implementation_state = implementation_state(implementation_process);
    let auto_review_state = if review_processes
        .iter()
        .any(|row| row.status == ExecutionProcessStatus::Running)
    {
        AutopilotDecision::Running
    } else {
        latest_review_decision.clone()
    };
    let review_fix_state = review_fix_state(review_fix_process);
    let pr_status = latest_pr_status(deployment, workspace).await;
    let pr_merge_state = infer_pr_merge_state(
        deployment,
        workspace,
        &latest_review_decision,
        pr_status.as_ref(),
    )
    .await?;
    let (next_action, blocker) = next_action(
        workspace,
        implementation_process,
        &latest_review_decision,
        auto_review_process,
        review_fix_process,
        &pr_merge_state,
        pr_status.as_ref(),
    );
    let (token_safety_state, token_safety_note) = token_safety(
        &next_action,
        blocker.as_deref(),
        implementation_process,
        auto_review_process,
        review_fix_process,
        &latest_review_decision,
    );
    let (workflow_state, workflow_state_reason) = workflow_state(
        &next_action,
        blocker.as_deref(),
        implementation_process,
        &latest_review_decision,
        auto_review_process,
        review_fix_process,
        &pr_merge_state,
    );

    Ok(ImplicationAutopilotStatus {
        workspace_id: workspace.id,
        workspace_name: workspace.name.clone(),
        implementation_state,
        auto_review_state,
        latest_review_decision: latest_review_decision.clone(),
        latest_review_excerpt,
        review_fix_state,
        pr_merge_state,
        next_action,
        blocker,
        implementation_process: implementation_process.map(process_summary),
        auto_review_process: auto_review_process.map(process_summary),
        review_fix_process: review_fix_process.map(process_summary),
        default_model: DEFAULT_CODEX_MODEL.to_string(),
        default_reasoning: DEFAULT_CODEX_REASONING.to_string(),
        daemonized: true,
        workflow_state,
        workflow_state_reason,
        duplicate_prevention_key: duplicate_prevention_key(
            workspace,
            implementation_process,
            auto_review_process,
            review_fix_process,
            &latest_review_decision,
        ),
        token_safety_state,
        token_safety_note,
        pr_status,
    })
}

async fn list_processes(
    pool: &sqlx::SqlitePool,
    workspace_id: Uuid,
) -> Result<Vec<ProcessRow>, sqlx::Error> {
    sqlx::query_as::<_, ProcessRow>(
        r#"SELECT
            ep.id,
            ep.session_id,
            s.name AS session_name,
            ep.status,
            ep.run_reason,
            ep.exit_code,
            ep.started_at AS started_at,
            ep.completed_at AS completed_at
        FROM execution_processes ep
        JOIN sessions s ON s.id = ep.session_id
        WHERE s.workspace_id = ? AND ep.run_reason != 'devserver' AND ep.dropped = FALSE
        ORDER BY ep.created_at DESC"#,
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await
}

fn process_summary(row: &ProcessRow) -> AutopilotProcessSummary {
    AutopilotProcessSummary {
        id: row.id,
        session_id: row.session_id,
        session_name: row.session_name.clone(),
        status: row.status.clone(),
        run_reason: row.run_reason.clone(),
        exit_code: row.exit_code,
        started_at: row.started_at.clone(),
        completed_at: row.completed_at.clone(),
    }
}

#[allow(clippy::too_many_arguments)]
fn process_summary_from_parts(
    id: Uuid,
    session_id: Uuid,
    session_name: Option<String>,
    status: ExecutionProcessStatus,
    run_reason: ExecutionProcessRunReason,
    exit_code: Option<i64>,
    started_at: String,
    completed_at: Option<String>,
) -> AutopilotProcessSummary {
    AutopilotProcessSummary {
        id,
        session_id,
        session_name,
        status,
        run_reason,
        exit_code,
        started_at,
        completed_at,
    }
}

async fn latest_pr_status(
    deployment: &DeploymentImpl,
    workspace: &Workspace,
) -> Option<AutopilotPrStatus> {
    let local_pr = PullRequest::find_by_workspace_id(&deployment.db().pool, workspace.id)
        .await
        .ok()
        .and_then(|prs| prs.into_iter().next());

    if let Some(pr) = local_pr {
        return Some(pr_status_from_url(&pr.pr_url).await.unwrap_or_else(|| {
            AutopilotPrStatus {
                number: pr.pr_number,
                url: pr.pr_url,
                state: format!("{:?}", pr.pr_status).to_ascii_lowercase(),
                is_draft: false,
                head_sha: None,
                base_branch: Some(pr.target_branch_name),
                mergeable: None,
                merge_state_status: None,
                merge_commit_sha: pr.merge_commit_sha,
                checks_state: AutopilotPrChecksState::Unknown,
                checks_summary:
                    "PR is tracked locally, but live GitHub check details are unavailable."
                        .to_string(),
                merge_blocker: Some(
                    "Live GitHub PR status is unavailable; cannot verify checks or mergeability."
                        .to_string(),
                ),
                source: "local".to_string(),
            }
        }));
    }

    let client = deployment.remote_client().ok()?;
    let remote_workspace = client.get_workspace_by_local_id(workspace.id).await.ok()?;
    let issue_id = remote_workspace.issue_id?;
    let issue = client.get_issue(issue_id).await.ok()?;
    let pr_url = latest_pr_url_from_issue_metadata(&issue.extension_metadata)?;
    pr_status_from_url(&pr_url).await
}

fn latest_pr_url_from_issue_metadata(metadata: &Value) -> Option<String> {
    metadata
        .get("github_link")
        .and_then(|link| link.get("latest_pr_url"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(ToString::to_string)
}

async fn pr_status_from_url(pr_url: &str) -> Option<AutopilotPrStatus> {
    let output = tokio::process::Command::new("gh")
        .args([
            "pr",
            "view",
            pr_url,
            "--json",
            "number,url,state,isDraft,headRefOid,baseRefName,mergeable,mergeStateStatus,mergeCommit,statusCheckRollup",
        ])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let value: Value = serde_json::from_slice(&output.stdout).ok()?;
    let number = value.get("number").and_then(Value::as_i64)?;
    let url = value
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or(pr_url)
        .to_string();
    let state = value
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("UNKNOWN")
        .to_ascii_lowercase();
    let is_draft = value
        .get("isDraft")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let head_sha = value
        .get("headRefOid")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let mergeable = value
        .get("mergeable")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let merge_state_status = value
        .get("mergeStateStatus")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let base_branch = value
        .get("baseRefName")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let merge_commit_sha = value
        .get("mergeCommit")
        .and_then(|commit| commit.get("oid"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let (checks_state, checks_summary) = summarize_check_rollup(value.get("statusCheckRollup"));
    let merge_blocker = pr_merge_blocker(
        is_draft,
        mergeable.as_deref(),
        merge_state_status.as_deref(),
        &checks_state,
    );

    Some(AutopilotPrStatus {
        number,
        url,
        state,
        is_draft,
        head_sha,
        base_branch,
        mergeable,
        merge_state_status,
        merge_commit_sha,
        checks_state,
        checks_summary,
        merge_blocker,
        source: "github".to_string(),
    })
}

fn summarize_check_rollup(rollup: Option<&Value>) -> (AutopilotPrChecksState, String) {
    let Some(checks) = rollup.and_then(Value::as_array) else {
        return (
            AutopilotPrChecksState::Unknown,
            "GitHub did not return check details.".to_string(),
        );
    };
    if checks.is_empty() {
        return (
            AutopilotPrChecksState::NoChecks,
            "No required or reported checks were found.".to_string(),
        );
    }

    let mut passing = 0usize;
    let mut pending = 0usize;
    let mut failing = Vec::new();

    for check in checks {
        let name = check_name(check);
        let conclusion = check
            .get("conclusion")
            .or_else(|| check.get("state"))
            .or_else(|| check.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN")
            .to_ascii_uppercase();
        match conclusion.as_str() {
            "SUCCESS" | "SKIPPED" | "NEUTRAL" | "COMPLETED" => passing += 1,
            "PENDING" | "QUEUED" | "REQUESTED" | "WAITING" | "IN_PROGRESS" | "EXPECTED" => {
                pending += 1
            }
            "FAILURE" | "FAILED" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" => {
                failing.push(name)
            }
            _ => pending += 1,
        }
    }

    if !failing.is_empty() {
        return (
            AutopilotPrChecksState::Failing,
            format!(
                "{} failing check(s): {}",
                failing.len(),
                failing.into_iter().take(3).collect::<Vec<_>>().join(", ")
            ),
        );
    }
    if pending > 0 {
        return (
            AutopilotPrChecksState::Pending,
            format!("{pending} pending check(s), {passing} passing."),
        );
    }
    (
        AutopilotPrChecksState::Passing,
        format!("{passing} check(s) passing."),
    )
}

fn check_name(check: &Value) -> String {
    ["name", "context", "workflowName"]
        .iter()
        .filter_map(|key| check.get(*key).and_then(Value::as_str))
        .find(|name| !name.trim().is_empty())
        .unwrap_or("unnamed check")
        .to_string()
}

fn pr_merge_blocker(
    is_draft: bool,
    mergeable: Option<&str>,
    merge_state_status: Option<&str>,
    checks_state: &AutopilotPrChecksState,
) -> Option<String> {
    if is_draft {
        return Some("PR is still a draft.".to_string());
    }
    if mergeable.is_none() {
        return Some("PR mergeability is unknown.".to_string());
    }
    if mergeable.is_some_and(|value| value.eq_ignore_ascii_case("CONFLICTING"))
        || merge_state_status.is_some_and(|value| {
            matches!(
                value.to_ascii_uppercase().as_str(),
                "DIRTY" | "UNKNOWN" | "BLOCKED" | "BEHIND"
            )
        })
    {
        return Some(format!(
            "PR is not currently mergeable ({}/{}).",
            mergeable.unwrap_or("unknown"),
            merge_state_status.unwrap_or("unknown")
        ));
    }
    match checks_state {
        AutopilotPrChecksState::Failing => Some("PR has failing checks.".to_string()),
        AutopilotPrChecksState::Pending | AutopilotPrChecksState::Unknown => {
            Some("PR checks are not green yet.".to_string())
        }
        AutopilotPrChecksState::NoChecks | AutopilotPrChecksState::Passing => None,
    }
}

fn is_auto_review_session(name: Option<&str>) -> bool {
    let normalized = name.unwrap_or_default().trim().to_ascii_lowercase();
    if is_review_fix_session(Some(&normalized)) {
        return false;
    }

    normalized == "auto review"
        || normalized.starts_with("auto review -")
        || normalized.starts_with("auto review rerun -")
}

fn is_review_fix_session(name: Option<&str>) -> bool {
    let normalized = name.unwrap_or_default().trim().to_ascii_lowercase();
    normalized.contains("review fix") || normalized.contains("fix review")
}

fn is_implementation_process(row: &ProcessRow) -> bool {
    row.run_reason == ExecutionProcessRunReason::CodingAgent
        && !is_auto_review_session(row.session_name.as_deref())
        && !is_review_fix_session(row.session_name.as_deref())
}

fn implementation_state(row: Option<&ProcessRow>) -> String {
    match row {
        None => "missing".to_string(),
        Some(row) if row.status == ExecutionProcessStatus::Running => "running".to_string(),
        Some(row)
            if row.status == ExecutionProcessStatus::Completed && row.exit_code == Some(0) =>
        {
            "completed".to_string()
        }
        Some(row) if row.status == ExecutionProcessStatus::Completed => "failed".to_string(),
        Some(row) => format!("{:?}", row.status).to_ascii_lowercase(),
    }
}

fn review_fix_state(row: Option<&ProcessRow>) -> String {
    match row {
        None => "not_started".to_string(),
        Some(row) if row.status == ExecutionProcessStatus::Running => "running".to_string(),
        Some(row)
            if row.status == ExecutionProcessStatus::Completed && row.exit_code == Some(0) =>
        {
            "completed".to_string()
        }
        Some(row) if row.status == ExecutionProcessStatus::Completed => "failed".to_string(),
        Some(row) => format!("{:?}", row.status).to_ascii_lowercase(),
    }
}

async fn decide_from_review_attempts<'a>(
    deployment: &DeploymentImpl,
    reviews: &[&'a ProcessRow],
) -> (AutopilotDecision, Option<String>, Option<&'a ProcessRow>) {
    decide_from_review_attempts_with_loader(reviews, |process_id| async move {
        process_agent_text(deployment, process_id).await
    })
    .await
}

async fn decide_from_review_attempts_with_loader<'a, F, Fut>(
    reviews: &[&'a ProcessRow],
    mut load_text: F,
) -> (AutopilotDecision, Option<String>, Option<&'a ProcessRow>)
where
    F: FnMut(Uuid) -> Fut,
    Fut: Future<Output = String>,
{
    if reviews.is_empty() {
        return (AutopilotDecision::Missing, None, None);
    }

    let latest = reviews[0];
    if latest.status == ExecutionProcessStatus::Running {
        return (AutopilotDecision::Running, None, Some(latest));
    }

    if latest.status == ExecutionProcessStatus::Completed && latest.exit_code == Some(0) {
        let text = load_text(latest.id).await;
        if text.trim().is_empty() {
            return (AutopilotDecision::Failed, None, Some(latest));
        }

        let decision = decision_from_text(&text);
        return (decision, Some(excerpt(&text)), Some(latest));
    }

    (AutopilotDecision::Failed, None, Some(latest))
}

#[cfg(test)]
fn running_review_attempt<'a>(reviews: &[&'a ProcessRow]) -> Option<&'a ProcessRow> {
    reviews
        .iter()
        .copied()
        .find(|row| row.status == ExecutionProcessStatus::Running)
}

async fn process_agent_text(deployment: &DeploymentImpl, process_id: Uuid) -> String {
    let Some(messages) = services::services::execution_process::load_raw_log_messages(
        &deployment.db().pool,
        process_id,
    )
    .await
    else {
        return String::new();
    };
    let lines = messages.into_iter().filter_map(|msg| match msg {
        LogMsg::Stdout(text) => Some(text),
        _ => None,
    });
    extract_process_agent_text(lines)
}

fn extract_process_agent_text(lines: impl IntoIterator<Item = String>) -> String {
    let mut item_phase = std::collections::HashMap::<String, String>::new();
    let mut item_type = std::collections::HashMap::<String, String>::new();
    let mut completed_text = std::collections::HashMap::<String, String>::new();
    let mut delta_text = std::collections::HashMap::<String, Vec<String>>::new();
    let mut final_ids = Vec::<String>::new();
    let mut review_ids = Vec::<String>::new();

    for chunk in lines {
        for inner_line in chunk.lines() {
            let Ok(msg) = serde_json::from_str::<Value>(inner_line) else {
                continue;
            };
            let method = msg
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            match method {
                "item/started" | "item/completed" => {
                    let item = params.get("item").unwrap_or(&Value::Null);
                    let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
                    let typ = item.get("type").and_then(Value::as_str).unwrap_or_default();
                    let phase = item
                        .get("phase")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !item_id.is_empty() {
                        item_type.insert(item_id.to_string(), typ.to_string());
                        item_phase.insert(item_id.to_string(), phase.to_string());
                        if phase == "final_answer" {
                            final_ids.push(item_id.to_string());
                        }
                        if typ == "review_rollout_assistant" || typ == "exitedReviewMode" {
                            review_ids.push(item_id.to_string());
                        }
                    }
                    let text = item_text(item);
                    if !item_id.is_empty() && !text.trim().is_empty() {
                        completed_text.insert(item_id.to_string(), text);
                    } else if (typ == "review_rollout_assistant" || typ == "exitedReviewMode")
                        && !text.trim().is_empty()
                    {
                        let anon = format!("anonymous-{}", review_ids.len());
                        completed_text.insert(anon.clone(), text);
                        review_ids.push(anon);
                    }
                }
                "item/agentMessage/delta" => {
                    let item_id = params
                        .get("itemId")
                        .or_else(|| params.get("item_id"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let delta = params
                        .get("delta")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !item_id.is_empty() && !delta.is_empty() {
                        delta_text
                            .entry(item_id.to_string())
                            .or_default()
                            .push(delta.to_string());
                    }
                }
                "review_rollout_assistant" => {
                    let item = params.get("item").unwrap_or(&params);
                    let text = text_from_value(item);
                    if !text.trim().is_empty() {
                        let review_id = format!("review-{}", completed_text.len());
                        completed_text.insert(review_id.clone(), text);
                        review_ids.push(review_id);
                    }
                }
                _ => {}
            }
        }
    }

    let text_for = |item_id: &str| {
        completed_text
            .get(item_id)
            .cloned()
            .or_else(|| delta_text.get(item_id).map(|parts| parts.join("")))
            .unwrap_or_default()
    };

    let final_text = final_ids
        .iter()
        .map(|id| text_for(id))
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if !final_text.trim().is_empty() {
        return final_text;
    }

    let review_text = review_ids
        .iter()
        .map(|id| text_for(id))
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if !review_text.trim().is_empty() {
        return review_text;
    }

    item_type
        .iter()
        .filter(|(id, typ)| {
            *typ == "agentMessage"
                && matches!(
                    item_phase.get(*id).map(String::as_str),
                    None | Some("") | Some("final_answer") | Some("commentary")
                )
        })
        .map(|(id, _)| text_for(id))
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn item_text(item: &Value) -> String {
    let typ = item.get("type").and_then(Value::as_str).unwrap_or_default();
    if typ == "agentMessage" {
        return text_from_value(item.get("text").unwrap_or(&Value::Null));
    }
    if typ == "review_rollout_assistant" || typ == "exitedReviewMode" {
        return text_from_value(item);
    }
    String::new()
}

fn text_from_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Array(values) => values
            .iter()
            .map(text_from_value)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(map) => [
            "text", "message", "body", "summary", "decision", "review", "content",
        ]
        .iter()
        .filter_map(|key| map.get(*key))
        .map(text_from_value)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n"),
        _ => String::new(),
    }
}

fn decision_from_text(text: &str) -> AutopilotDecision {
    let normalized = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let asks_for_changes = normalized.contains("decision: request")
        || normalized.contains("request changes")
        || normalized.contains("changes requested")
        || normalized.contains("blocking regression")
        || normalized.contains("blocker:");
    let negates_blockers = normalized.contains("no blockers")
        || normalized.contains("no blocking")
        || normalized.contains("no blocking regressions");
    if asks_for_changes && !negates_blockers {
        return AutopilotDecision::RequestChanges;
    }
    if normalized.contains("decision: pass")
        || normalized.contains("decision: approve")
        || normalized.contains("approved")
        || normalized.contains("no blockers")
        || normalized.contains("no blocking regressions")
        || normalized.contains("no blocking issues")
    {
        return AutopilotDecision::Pass;
    }
    AutopilotDecision::Failed
}

fn excerpt(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    compact.chars().take(280).collect()
}

async fn infer_pr_merge_state(
    deployment: &DeploymentImpl,
    workspace: &Workspace,
    decision: &AutopilotDecision,
    pr_status: Option<&AutopilotPrStatus>,
) -> Result<String, ApiError> {
    if workspace.archived || workspace.worktree_deleted {
        return Ok("done_or_archived".to_string());
    }

    if decision != &AutopilotDecision::Pass {
        return Ok(match decision {
            AutopilotDecision::RequestChanges => "blocked_by_review".to_string(),
            AutopilotDecision::Running => "waiting_for_review".to_string(),
            AutopilotDecision::Missing => "waiting_for_review".to_string(),
            AutopilotDecision::Failed => "review_failed".to_string(),
            AutopilotDecision::Pass => unreachable!(),
        });
    }

    if let Some(pr_status) = pr_status {
        if pr_status.state.eq_ignore_ascii_case("merged") {
            return Ok("merged_pending_done".to_string());
        }
        if let Some(blocker) = pr_status.merge_blocker.as_deref() {
            if blocker.contains("draft") {
                return Ok("blocked_by_draft_pr".to_string());
            }
            if blocker.contains("failing checks") {
                return Ok("blocked_by_failing_checks".to_string());
            }
            if blocker.contains("checks are not green") {
                return Ok("waiting_for_checks".to_string());
            }
            return Ok("blocked_by_pr_mergeability".to_string());
        }
    }

    let merges = Merge::find_by_workspace_id(&deployment.db().pool, workspace.id).await?;
    let pr_statuses = merges
        .iter()
        .filter_map(|merge| match merge {
            Merge::Pr(pr) => Some(pr.pr_info.status.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();

    if pr_statuses
        .iter()
        .any(|status| matches!(status, MergeStatus::Open))
    {
        if workspace_has_dirty_or_conflicting_changes(deployment, workspace).await? {
            return Ok("blocked_by_dirty_worktree".to_string());
        }
        return Ok("pr_open_pending_merge".to_string());
    }

    if let Some(state) = merge_state_from_pr_completion(&pr_statuses) {
        if state == "merged_pending_done"
            && workspace.pinned
            && linked_issue_is_done(deployment, workspace).await?
        {
            return Ok("done_or_archived".to_string());
        }
        return Ok(state.to_string());
    }

    if merges.iter().any(|merge| matches!(merge, Merge::Direct(_))) {
        if workspace.pinned && linked_issue_is_done(deployment, workspace).await? {
            return Ok("done_or_archived".to_string());
        }
        return Ok("merged_pending_done".to_string());
    }

    Ok("no_pr_merge_found".to_string())
}

fn merge_state_from_pr_completion(statuses: &[MergeStatus]) -> Option<&'static str> {
    if statuses.is_empty() {
        return None;
    }
    if linked_pr_merge_completion_blocker(statuses).is_none() {
        return Some("merged_pending_done");
    }
    Some("blocked_by_pr_requirements")
}

async fn linked_issue_is_done(
    deployment: &DeploymentImpl,
    workspace: &Workspace,
) -> Result<bool, ApiError> {
    let client = deployment.remote_client()?;
    let remote_workspace = client.get_workspace_by_local_id(workspace.id).await?;
    let Some(issue_id) = remote_workspace.issue_id else {
        return Ok(false);
    };
    let issue = client.get_issue(issue_id).await?;
    Ok(issue.completed_at.is_some())
}

async fn workspace_has_dirty_or_conflicting_changes(
    deployment: &DeploymentImpl,
    workspace: &Workspace,
) -> Result<bool, ApiError> {
    let Some(container_ref) = workspace.container_ref.as_deref() else {
        return Ok(false);
    };
    let workspace_path = PathBuf::from(container_ref);
    let repos =
        WorkspaceRepo::find_repos_for_workspace(&deployment.db().pool, workspace.id).await?;

    for repo in repos {
        let worktree_path = workspace_path.join(&repo.name);
        let conflicted_files = deployment.git().get_conflicted_files(&worktree_path)?;
        if !conflicted_files.is_empty() || deployment.git().is_rebase_in_progress(&worktree_path)? {
            return Ok(true);
        }

        let (uncommitted_count, untracked_count) = deployment
            .git()
            .get_worktree_change_counts(&worktree_path)?;
        if uncommitted_count > 0 || untracked_count > 0 {
            return Ok(true);
        }
    }

    Ok(false)
}

fn review_fix_start_gate(
    workspace: &Workspace,
    implementation: Option<&ProcessRow>,
    decision: &AutopilotDecision,
    latest_review: Option<&ProcessRow>,
    review_fix: Option<&ProcessRow>,
) -> Result<(), String> {
    if workspace.archived || workspace.worktree_deleted {
        return Err("Workspace is archived or deleted.".to_string());
    }
    let Some(implementation) = implementation else {
        return Err("No implementation process was found for this workspace.".to_string());
    };
    if implementation.status == ExecutionProcessStatus::Running {
        return Err("Implementation is still running.".to_string());
    }
    if implementation.status != ExecutionProcessStatus::Completed
        || implementation.exit_code != Some(0)
    {
        return Err("Latest implementation process did not complete cleanly.".to_string());
    }
    if decision != &AutopilotDecision::RequestChanges {
        return Err("Review fix can only start after auto-review requests changes.".to_string());
    }
    if let Some(row) = review_fix {
        if row.status == ExecutionProcessStatus::Running {
            return Err("Review fix is already running.".to_string());
        }
        if review_fix_completed_after_review(row, latest_review) {
            return Err(
                "Review fix already completed after the latest review; rerun auto-review next."
                    .to_string(),
            );
        }
        if review_fix_attempted_after_review(row, latest_review) {
            return Err(
                "Review fix already ran after the latest review and did not complete cleanly."
                    .to_string(),
            );
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn auto_review_start_gate(
    workspace: &Workspace,
    implementation: Option<&ProcessRow>,
    decision: &AutopilotDecision,
    latest_review: Option<&ProcessRow>,
    review_fix: Option<&ProcessRow>,
    pr_merge_state: &str,
    pr_status: Option<&AutopilotPrStatus>,
    rerun: bool,
) -> Result<(), String> {
    let (action, blocker) = next_action(
        workspace,
        implementation,
        decision,
        latest_review,
        review_fix,
        pr_merge_state,
        pr_status,
    );

    if action != AutopilotNextAction::StartAutoReview {
        return Err(format!(
            "Auto-review cannot start while the next action is {}.",
            next_action_name(&action)
        ));
    }

    if decision == &AutopilotDecision::RequestChanges {
        let review_fix_completed =
            review_fix.is_some_and(|row| review_fix_completed_after_review(row, latest_review));
        if review_fix_completed && !rerun {
            return Err(
                "Auto-review rerun must be explicit after review fix completes.".to_string(),
            );
        }
    }

    if let Some(blocker) = blocker
        && !rerun
    {
        return Err(blocker);
    }

    Ok(())
}

fn next_action_name(action: &AutopilotNextAction) -> &'static str {
    match action {
        AutopilotNextAction::NoWorkspace => "no_workspace",
        AutopilotNextAction::WaitForImplementation => "wait_for_implementation",
        AutopilotNextAction::StartAutoReview => "start_auto_review",
        AutopilotNextAction::WaitForAutoReview => "wait_for_auto_review",
        AutopilotNextAction::StartReviewFix => "start_review_fix",
        AutopilotNextAction::WaitForReviewFix => "wait_for_review_fix",
        AutopilotNextAction::ReadyForMerge => "ready_for_merge",
        AutopilotNextAction::MergeWait => "merge_wait",
        AutopilotNextAction::Done => "done",
        AutopilotNextAction::InvestigateFailure => "investigate_failure",
    }
}

fn token_safety(
    next_action: &AutopilotNextAction,
    blocker: Option<&str>,
    implementation: Option<&ProcessRow>,
    auto_review: Option<&ProcessRow>,
    review_fix: Option<&ProcessRow>,
    decision: &AutopilotDecision,
) -> (AutopilotTokenSafetyState, String) {
    let running_process = [implementation, auto_review, review_fix]
        .into_iter()
        .flatten()
        .any(|row| row.status == ExecutionProcessStatus::Running);

    if running_process {
        return (
            AutopilotTokenSafetyState::Guarded,
            "A single agent session is running. Start controls stay hidden until the server reports the next safe action.".to_string(),
        );
    }

    if matches!(
        next_action,
        AutopilotNextAction::NoWorkspace | AutopilotNextAction::InvestigateFailure
    ) {
        return (
            AutopilotTokenSafetyState::Blocked,
            blocker
                .unwrap_or("Autopilot is blocked before any new Codex session can start.")
                .to_string(),
        );
    }

    if next_action == &AutopilotNextAction::StartAutoReview
        && decision == &AutopilotDecision::RequestChanges
    {
        return (
            AutopilotTokenSafetyState::Guarded,
            "Auto-review reruns are guarded: the server only exposes this explicit rerun after a completed review-fix and while no agent is running.".to_string(),
        );
    }

    if next_action == &AutopilotNextAction::StartAutoReview {
        return (
            AutopilotTokenSafetyState::Guarded,
            "Auto-review starts are guarded: the server blocks duplicate running processes and repeated reruns without a review-fix or updated workflow state.".to_string(),
        );
    }

    (
        AutopilotTokenSafetyState::Idle,
        "No Codex review or fix session is running. Completed sessions with unseen output are idle and are not spending tokens.".to_string(),
    )
}

fn workflow_state(
    next_action: &AutopilotNextAction,
    blocker: Option<&str>,
    _implementation: Option<&ProcessRow>,
    decision: &AutopilotDecision,
    latest_review: Option<&ProcessRow>,
    review_fix: Option<&ProcessRow>,
    _pr_merge_state: &str,
) -> (AutopilotWorkflowState, String) {
    match next_action {
        AutopilotNextAction::NoWorkspace => (
            AutopilotWorkflowState::Queued,
            blocker
                .unwrap_or("Queued until a local workspace is linked.")
                .to_string(),
        ),
        AutopilotNextAction::WaitForImplementation => (
            AutopilotWorkflowState::ImplementationRunning,
            "Implementation session is running; no duplicate workspace or review will start."
                .to_string(),
        ),
        AutopilotNextAction::StartAutoReview => {
            if decision == &AutopilotDecision::RequestChanges
                && review_fix
                    .is_some_and(|row| review_fix_completed_after_review(row, latest_review))
            {
                (
                    AutopilotWorkflowState::ReadyToAdvance,
                    "Review fix completed; app advance may start exactly one re-review."
                        .to_string(),
                )
            } else {
                (
                    AutopilotWorkflowState::ReadyToAdvance,
                    "Implementation completed; app advance may promote to In review and start exactly one auto-review.".to_string(),
                )
            }
        }
        AutopilotNextAction::WaitForAutoReview => (
            AutopilotWorkflowState::ReviewRunning,
            "One auto-review is already running for this workspace state.".to_string(),
        ),
        AutopilotNextAction::StartReviewFix => (
            AutopilotWorkflowState::ReviewRequestedChanges,
            "Latest review requested changes; app advance may start exactly one review-fix session."
                .to_string(),
        ),
        AutopilotNextAction::WaitForReviewFix => (
            AutopilotWorkflowState::ReviewFixRunning,
            "One review-fix session is already running.".to_string(),
        ),
        AutopilotNextAction::ReadyForMerge => (
            AutopilotWorkflowState::ReviewPassed,
            blocker
                .unwrap_or("Review passed; app advance owns merge/done reconciliation.")
                .to_string(),
        ),
        AutopilotNextAction::MergeWait => (
            AutopilotWorkflowState::MergeWaiting,
            "Merge/check work is waiting.".to_string(),
        ),
        AutopilotNextAction::Done => (
            AutopilotWorkflowState::Done,
            "Workspace is done or archived.".to_string(),
        ),
        AutopilotNextAction::InvestigateFailure => (
            AutopilotWorkflowState::Blocked,
            blocker
                .unwrap_or("Autopilot is blocked before another Codex session can start.")
                .to_string(),
        ),
    }
}

fn duplicate_prevention_key(
    workspace: &Workspace,
    implementation: Option<&ProcessRow>,
    auto_review: Option<&ProcessRow>,
    review_fix: Option<&ProcessRow>,
    decision: &AutopilotDecision,
) -> String {
    format!(
        "{}:{}:{}:{}:{:?}",
        workspace.id,
        implementation
            .map(|row| row.id.to_string())
            .unwrap_or_else(|| "no-implementation".to_string()),
        auto_review
            .map(|row| row.id.to_string())
            .unwrap_or_else(|| "no-review".to_string()),
        review_fix
            .map(|row| row.id.to_string())
            .unwrap_or_else(|| "no-review-fix".to_string()),
        decision
    )
}

fn review_fix_completed_after_review(
    review_fix: &ProcessRow,
    latest_review: Option<&ProcessRow>,
) -> bool {
    review_fix.status == ExecutionProcessStatus::Completed
        && review_fix.exit_code == Some(0)
        && review_fix_attempted_after_review(review_fix, latest_review)
}

fn review_fix_attempted_after_review(
    review_fix: &ProcessRow,
    latest_review: Option<&ProcessRow>,
) -> bool {
    let Some(latest_review) = latest_review else {
        return false;
    };

    process_time_after(&review_fix.started_at, review_reference_time(latest_review))
}

fn review_reference_time(review: &ProcessRow) -> &str {
    review.completed_at.as_deref().unwrap_or(&review.started_at)
}

fn process_time_after(candidate: &str, reference: &str) -> bool {
    let candidate_time = DateTime::parse_from_rfc3339(candidate);
    let reference_time = DateTime::parse_from_rfc3339(reference);
    match (candidate_time, reference_time) {
        (Ok(candidate_time), Ok(reference_time)) => candidate_time > reference_time,
        _ => candidate > reference,
    }
}

fn next_action(
    workspace: &Workspace,
    implementation: Option<&ProcessRow>,
    decision: &AutopilotDecision,
    latest_review: Option<&ProcessRow>,
    review_fix: Option<&ProcessRow>,
    pr_merge_state: &str,
    _pr_status: Option<&AutopilotPrStatus>,
) -> (AutopilotNextAction, Option<String>) {
    if workspace.archived
        || workspace.worktree_deleted
        || pr_merge_state == "done_or_archived"
        || pr_merge_state == "merged"
    {
        return (AutopilotNextAction::Done, None);
    }
    let Some(implementation) = implementation else {
        return (
            AutopilotNextAction::NoWorkspace,
            Some("No implementation process was found for this workspace.".to_string()),
        );
    };
    if implementation.status == ExecutionProcessStatus::Running {
        return (AutopilotNextAction::WaitForImplementation, None);
    }
    if implementation.status != ExecutionProcessStatus::Completed
        || implementation.exit_code != Some(0)
    {
        return (
            AutopilotNextAction::InvestigateFailure,
            Some("Latest implementation process did not complete cleanly.".to_string()),
        );
    }
    match decision {
        AutopilotDecision::Missing => (AutopilotNextAction::StartAutoReview, None),
        AutopilotDecision::Running => (AutopilotNextAction::WaitForAutoReview, None),
        AutopilotDecision::Failed => (
            AutopilotNextAction::InvestigateFailure,
            Some(
                "Auto-review completed without a usable pass/request-changes decision.".to_string(),
            ),
        ),
        AutopilotDecision::RequestChanges => match review_fix {
            Some(row) if row.status == ExecutionProcessStatus::Running => {
                (AutopilotNextAction::WaitForReviewFix, None)
            }
            Some(row) if review_fix_completed_after_review(row, latest_review) => (
                AutopilotNextAction::StartAutoReview,
                Some("Review fix completed; rerun auto-review.".to_string()),
            ),
            Some(row) if review_fix_attempted_after_review(row, latest_review) => (
                AutopilotNextAction::InvestigateFailure,
                Some("Review fix did not complete cleanly.".to_string()),
            ),
            _ => (AutopilotNextAction::StartReviewFix, None),
        },
        AutopilotDecision::Pass => match merge_plan_from_state(pr_merge_state) {
            AutopilotMergePlan::Blocked(blocker)
                if pr_merge_state == "blocked_by_dirty_worktree"
                    || pr_merge_state == "blocked_by_draft_pr"
                    || pr_merge_state == "blocked_by_failing_checks"
                    || pr_merge_state == "blocked_by_pr_mergeability" =>
            {
                (AutopilotNextAction::InvestigateFailure, Some(blocker))
            }
            AutopilotMergePlan::Blocked(blocker) if pr_merge_state == "waiting_for_checks" => {
                (AutopilotNextAction::MergeWait, Some(blocker))
            }
            AutopilotMergePlan::Blocked(blocker) => {
                (AutopilotNextAction::ReadyForMerge, Some(blocker))
            }
            AutopilotMergePlan::AttemptPrMerge => (
                AutopilotNextAction::ReadyForMerge,
                Some(
                    "Review passed; app advance will attempt the linked PR merge and report GitHub blockers."
                        .to_string(),
                ),
            ),
            AutopilotMergePlan::ReconcileDone => (
                AutopilotNextAction::ReadyForMerge,
                Some("Review passed; app advance will reconcile merged PR state to Done.".to_string()),
            ),
        },
    }
}

fn dependency_blockers_for(
    issue_id: Uuid,
    done_issue_ids: &HashSet<Uuid>,
    relationships: &[BatchAdvanceRelationship],
) -> Vec<Uuid> {
    relationships
        .iter()
        .filter(|relationship| {
            relationship.issue_id == issue_id
                && !done_issue_ids.contains(&relationship.blocking_issue_id)
        })
        .map(|relationship| relationship.blocking_issue_id)
        .collect()
}

fn plan_batch_advance(
    candidates: &[BatchCandidate],
    relationships: &[BatchAdvanceRelationship],
    max_active: usize,
) -> BatchAdvancePlan {
    let done_issue_ids = candidates
        .iter()
        .filter(|candidate| candidate.done)
        .map(|candidate| candidate.issue_id)
        .collect::<HashSet<_>>();
    let mut active_count = candidates
        .iter()
        .filter(|candidate| candidate.active)
        .count();
    let max_active = max_active.max(1);

    let cards = candidates
        .iter()
        .map(|candidate| {
            let blockers = candidate
                .blockers
                .iter()
                .copied()
                .filter(|blocker| !done_issue_ids.contains(blocker))
                .chain(dependency_blockers_for(
                    candidate.issue_id,
                    &done_issue_ids,
                    relationships,
                ))
                .collect::<HashSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();

            if candidate.done {
                return BatchAdvanceCard {
                    issue_id: candidate.issue_id,
                    workspace_id: candidate.workspace_id,
                    state: BatchCardQueueState::Done,
                    blockers,
                    action: None,
                    reason: "Card is already done.".to_string(),
                };
            }

            if candidate.active {
                return BatchAdvanceCard {
                    issue_id: candidate.issue_id,
                    workspace_id: candidate.workspace_id,
                    state: BatchCardQueueState::Active,
                    blockers,
                    action: candidate.next_action.clone(),
                    reason: "Card already has active implementation, review, fix, or merge work."
                        .to_string(),
                };
            }

            if !blockers.is_empty() {
                return BatchAdvanceCard {
                    issue_id: candidate.issue_id,
                    workspace_id: candidate.workspace_id,
                    state: BatchCardQueueState::BlockedByDependencies,
                    blockers,
                    action: None,
                    reason: "Card is staged, but dependency blockers must finish before it starts."
                        .to_string(),
                };
            }

            if active_count >= max_active {
                return BatchAdvanceCard {
                    issue_id: candidate.issue_id,
                    workspace_id: candidate.workspace_id,
                    state: BatchCardQueueState::Queued,
                    blockers,
                    action: None,
                    reason: "Card is staged and waiting for the active cap.".to_string(),
                };
            }

            active_count += 1;
            BatchAdvanceCard {
                issue_id: candidate.issue_id,
                workspace_id: candidate.workspace_id,
                state: BatchCardQueueState::Runnable,
                blockers,
                action: candidate.next_action.clone(),
                reason: "Card is staged and dependency-unblocked under the active cap.".to_string(),
            }
        })
        .collect();

    BatchAdvancePlan {
        max_active,
        active_count: candidates
            .iter()
            .filter(|candidate| candidate.active)
            .count(),
        cards,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> Workspace {
        let now = Utc::now();
        Workspace {
            id: Uuid::new_v4(),
            task_id: Some(Uuid::new_v4()),
            container_ref: None,
            branch: "issue-264".to_string(),
            setup_completed_at: Some(now),
            created_at: now,
            updated_at: now,
            archived: false,
            pinned: false,
            name: Some("Issue 264".to_string()),
            worktree_deleted: false,
        }
    }

    fn process(name: &str, status: ExecutionProcessStatus, exit_code: Option<i64>) -> ProcessRow {
        ProcessRow {
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            session_name: Some(name.to_string()),
            status,
            run_reason: ExecutionProcessRunReason::CodingAgent,
            exit_code,
            started_at: Utc::now().to_rfc3339(),
            completed_at: Some(Utc::now().to_rfc3339()),
        }
    }

    fn completed_process(name: &str, exit_code: i64) -> ProcessRow {
        process(name, ExecutionProcessStatus::Completed, Some(exit_code))
    }

    fn completed_process_at(
        name: &str,
        exit_code: i64,
        started_at: &str,
        completed_at: &str,
    ) -> ProcessRow {
        ProcessRow {
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            session_name: Some(name.to_string()),
            status: ExecutionProcessStatus::Completed,
            run_reason: ExecutionProcessRunReason::CodingAgent,
            exit_code: Some(exit_code),
            started_at: started_at.to_string(),
            completed_at: Some(completed_at.to_string()),
        }
    }

    fn failed_process_at(name: &str, started_at: &str, completed_at: &str) -> ProcessRow {
        ProcessRow {
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            session_name: Some(name.to_string()),
            status: ExecutionProcessStatus::Failed,
            run_reason: ExecutionProcessRunReason::CodingAgent,
            exit_code: Some(1),
            started_at: started_at.to_string(),
            completed_at: Some(completed_at.to_string()),
        }
    }

    fn relationship(issue_id: Uuid, blocker: Uuid) -> BatchAdvanceRelationship {
        BatchAdvanceRelationship {
            issue_id,
            blocking_issue_id: blocker,
        }
    }

    fn pr_status(
        checks_state: AutopilotPrChecksState,
        merge_blocker: Option<&str>,
    ) -> AutopilotPrStatus {
        AutopilotPrStatus {
            number: 123,
            url: "https://github.com/gavinanelson/implication/pull/123".to_string(),
            state: "open".to_string(),
            is_draft: false,
            head_sha: Some("abc123".to_string()),
            base_branch: Some("main".to_string()),
            mergeable: Some("MERGEABLE".to_string()),
            merge_state_status: Some("CLEAN".to_string()),
            merge_commit_sha: None,
            checks_state,
            checks_summary: "checks summary".to_string(),
            merge_blocker: merge_blocker.map(ToString::to_string),
            source: "test".to_string(),
        }
    }

    #[test]
    fn running_review_attempt_returns_active_review_process() {
        let completed = completed_process("Auto review - Codex (medium)", 0);
        let running = process(
            "Auto review rerun - Codex (medium)",
            ExecutionProcessStatus::Running,
            None,
        );
        let reviews = vec![&completed, &running];

        assert_eq!(
            running_review_attempt(&reviews).map(|row| row.id),
            Some(running.id)
        );
    }

    #[test]
    fn review_fix_sessions_are_not_auto_review_sessions() {
        assert!(is_auto_review_session(Some("Auto review - Codex (medium)")));
        assert!(is_auto_review_session(Some(
            "Auto review rerun - Codex (medium)"
        )));
        assert_eq!(
            auto_review_session_name(false),
            "Auto review - Codex (medium)"
        );
        assert_eq!(
            auto_review_session_name(true),
            "Auto review rerun - Codex (medium)"
        );
        assert_eq!(review_fix_session_name(), "Review fix - Codex (medium)");
        assert!(is_review_fix_session(Some("Review fix - Codex (medium)")));
        assert!(!is_auto_review_session(Some("Review fix - Codex (medium)")));
        assert!(
            !review_fix_session_name()
                .to_ascii_lowercase()
                .starts_with("auto review")
        );
    }

    #[test]
    fn running_review_fix_is_not_a_running_auto_review_attempt() {
        let completed_review = completed_process("Auto review - Codex (medium)", 0);
        let running_review_fix = process(
            "Review fix - Codex (medium)",
            ExecutionProcessStatus::Running,
            None,
        );
        let rows = [&running_review_fix, &completed_review];
        let review_processes = rows
            .iter()
            .copied()
            .filter(|row| is_auto_review_session(row.session_name.as_deref()))
            .collect::<Vec<_>>();

        assert_eq!(review_processes.len(), 1);
        assert_eq!(review_processes[0].id, completed_review.id);
        assert!(running_review_attempt(&review_processes).is_none());
    }

    #[test]
    fn decision_prefers_request_changes_when_blockers_present() {
        assert_eq!(
            decision_from_text("Decision: request changes\nBlocker: test fails"),
            AutopilotDecision::RequestChanges
        );
    }

    #[test]
    fn decision_treats_no_blockers_as_pass() {
        assert_eq!(
            decision_from_text("Decision: pass\nNo blocking regressions found."),
            AutopilotDecision::Pass
        );
    }

    #[test]
    fn extract_process_agent_text_prefers_final_answer() {
        let line = serde_json::json!({
            "method": "item/completed",
            "params": {
                "item": {
                    "id": "final-1",
                    "type": "agentMessage",
                    "phase": "final_answer",
                    "text": "Decision: pass\nNo blockers."
                }
            }
        })
        .to_string();
        assert_eq!(
            extract_process_agent_text([line]),
            "Decision: pass\nNo blockers."
        );
    }

    #[test]
    fn extract_process_agent_text_reads_review_rollout_payloads() {
        let line = serde_json::json!({
            "method": "review_rollout_assistant",
            "params": {"decision": "Decision: request changes", "summary": "Blocker: missing test"}
        })
        .to_string();
        assert_eq!(
            extract_process_agent_text([line]),
            "Blocker: missing test\nDecision: request changes"
        );
    }

    #[tokio::test]
    async fn latest_empty_review_attempt_is_failed_instead_of_falling_back_to_older_pass() {
        let latest_empty = completed_process("Auto review rerun - Codex (medium)", 0);
        let older_pass = completed_process("Auto review - Codex (medium)", 0);
        let reviews = vec![&latest_empty, &older_pass];

        let (decision, excerpt, row) =
            decide_from_review_attempts_with_loader(&reviews, |process_id| async move {
                if process_id == older_pass.id {
                    "Decision: pass\nNo blockers.".to_string()
                } else {
                    String::new()
                }
            })
            .await;

        assert_eq!(decision, AutopilotDecision::Failed);
        assert_eq!(excerpt, None);
        assert_eq!(row.map(|row| row.id), Some(latest_empty.id));
    }

    #[tokio::test]
    async fn latest_failed_review_attempt_is_failed_instead_of_falling_back_to_older_pass() {
        let latest_failed = completed_process("Auto review rerun - Codex (medium)", 1);
        let older_pass = completed_process("Auto review - Codex (medium)", 0);
        let reviews = vec![&latest_failed, &older_pass];

        let (decision, excerpt, row) =
            decide_from_review_attempts_with_loader(&reviews, |process_id| async move {
                if process_id == older_pass.id {
                    "Decision: pass\nNo blockers.".to_string()
                } else {
                    String::new()
                }
            })
            .await;

        assert_eq!(decision, AutopilotDecision::Failed);
        assert_eq!(excerpt, None);
        assert_eq!(row.map(|row| row.id), Some(latest_failed.id));
    }

    #[test]
    fn implication_eligibility_reads_repo_from_issue_metadata() {
        let metadata = serde_json::json!({
            "github_link": {
                "repo_full_name": "gavinanelson/implication",
                "issue_number": 264,
                "issue_url": "https://github.com/gavinanelson/implication/issues/264"
            }
        });

        assert!(is_implication_issue_metadata(&metadata));

        let spoofed_payload_repo = "gavinanelson/implication";
        let metadata = serde_json::json!({
            "github_link": {
                "repo_full_name": "gavinanelson/vibe-kanban",
                "issue_number": 2,
                "issue_url": "https://github.com/gavinanelson/vibe-kanban/issues/2"
            }
        });

        assert_eq!(spoofed_payload_repo, "gavinanelson/implication");
        assert!(!is_implication_issue_metadata(&metadata));
    }

    #[test]
    fn action_gating_allows_review_fix_only_after_clean_implementation_and_requested_changes() {
        let implementation = completed_process("Implementation", 0);
        let workspace = workspace();

        assert_eq!(
            review_fix_start_gate(
                &workspace,
                Some(&implementation),
                &AutopilotDecision::RequestChanges,
                None,
                None
            ),
            Ok(())
        );
    }

    #[test]
    fn action_gating_blocks_review_fix_without_requested_changes() {
        let implementation = completed_process("Implementation", 0);
        let workspace = workspace();

        assert_eq!(
            review_fix_start_gate(
                &workspace,
                Some(&implementation),
                &AutopilotDecision::Pass,
                None,
                None
            ),
            Err("Review fix can only start after auto-review requests changes.".to_string())
        );
    }

    #[test]
    fn eligibility_requires_implication_repo_and_linked_issue() {
        assert!(!is_implication_issue_metadata(&serde_json::json!({})));
        assert!(!is_implication_issue_metadata(&serde_json::json!({
            "github_link": {
                "issue_number": 264,
                "issue_url": "https://github.com/gavinanelson/implication/issues/264"
            }
        })));
    }

    #[test]
    fn action_gating_allows_initial_auto_review_when_next_action_starts_review() {
        let implementation = completed_process("Implementation", 0);
        let workspace = workspace();

        assert_eq!(
            auto_review_start_gate(
                &workspace,
                Some(&implementation),
                &AutopilotDecision::Missing,
                None,
                None,
                "waiting_for_review",
                None,
                false,
            ),
            Ok(())
        );
    }

    #[test]
    fn action_gating_requires_explicit_rerun_after_review_fix_completes() {
        let implementation = completed_process("Implementation", 0);
        let review = completed_process_at(
            "Auto review - Codex (medium)",
            0,
            "2026-04-25T12:00:00Z",
            "2026-04-25T12:10:00Z",
        );
        let review_fix = completed_process_at(
            "Review fix",
            0,
            "2026-04-25T12:20:00Z",
            "2026-04-25T12:30:00Z",
        );
        let workspace = workspace();

        assert_eq!(
            auto_review_start_gate(
                &workspace,
                Some(&implementation),
                &AutopilotDecision::RequestChanges,
                Some(&review),
                Some(&review_fix),
                "blocked_by_review",
                None,
                false,
            ),
            Err("Auto-review rerun must be explicit after review fix completes.".to_string())
        );

        assert_eq!(
            auto_review_start_gate(
                &workspace,
                Some(&implementation),
                &AutopilotDecision::RequestChanges,
                Some(&review),
                Some(&review_fix),
                "blocked_by_review",
                None,
                true,
            ),
            Ok(())
        );
    }

    #[test]
    fn action_gating_blocks_auto_review_when_review_fix_is_next() {
        let implementation = completed_process("Implementation", 0);
        let review = completed_process("Auto review - Codex (medium)", 0);
        let workspace = workspace();

        assert_eq!(
            auto_review_start_gate(
                &workspace,
                Some(&implementation),
                &AutopilotDecision::RequestChanges,
                Some(&review),
                None,
                "blocked_by_review",
                None,
                false,
            ),
            Err("Auto-review cannot start while the next action is start_review_fix.".to_string())
        );
    }

    #[test]
    fn latest_review_requires_a_newer_review_fix_before_re_review() {
        let implementation = completed_process("Implementation", 0);
        let latest_review = completed_process_at(
            "Auto review rerun - Codex (medium)",
            0,
            "2026-04-25T12:40:00Z",
            "2026-04-25T12:50:00Z",
        );
        let old_review_fix = completed_process_at(
            "Review fix - Codex (medium)",
            0,
            "2026-04-25T12:20:00Z",
            "2026-04-25T12:30:00Z",
        );
        let workspace = workspace();

        let (next_action, blocker) = next_action(
            &workspace,
            Some(&implementation),
            &AutopilotDecision::RequestChanges,
            Some(&latest_review),
            Some(&old_review_fix),
            "blocked_by_review",
            None,
        );

        assert_eq!(next_action, AutopilotNextAction::StartReviewFix);
        assert_eq!(blocker, None);
        assert_eq!(
            auto_review_start_gate(
                &workspace,
                Some(&implementation),
                &AutopilotDecision::RequestChanges,
                Some(&latest_review),
                Some(&old_review_fix),
                "blocked_by_review",
                None,
                true,
            ),
            Err("Auto-review cannot start while the next action is start_review_fix.".to_string())
        );
    }

    #[test]
    fn review_fix_gate_blocks_duplicate_fix_after_latest_review() {
        let implementation = completed_process("Implementation", 0);
        let latest_review = completed_process_at(
            "Auto review - Codex (medium)",
            0,
            "2026-04-25T12:00:00Z",
            "2026-04-25T12:10:00Z",
        );
        let review_fix = completed_process_at(
            "Review fix - Codex (medium)",
            0,
            "2026-04-25T12:20:00Z",
            "2026-04-25T12:30:00Z",
        );
        let workspace = workspace();

        assert_eq!(
            review_fix_start_gate(
                &workspace,
                Some(&implementation),
                &AutopilotDecision::RequestChanges,
                Some(&latest_review),
                Some(&review_fix),
            ),
            Err(
                "Review fix already completed after the latest review; rerun auto-review next."
                    .to_string()
            )
        );
    }

    #[test]
    fn failed_review_fix_after_latest_review_surfaces_blocker() {
        let implementation = completed_process("Implementation", 0);
        let latest_review = completed_process_at(
            "Auto review - Codex (medium)",
            0,
            "2026-04-25T12:00:00Z",
            "2026-04-25T12:10:00Z",
        );
        let failed_review_fix = failed_process_at(
            "Review fix - Codex (medium)",
            "2026-04-25T12:20:00Z",
            "2026-04-25T12:30:00Z",
        );
        let workspace = workspace();

        let (next_action, blocker) = next_action(
            &workspace,
            Some(&implementation),
            &AutopilotDecision::RequestChanges,
            Some(&latest_review),
            Some(&failed_review_fix),
            "blocked_by_review",
            None,
        );

        assert_eq!(next_action, AutopilotNextAction::InvestigateFailure);
        assert_eq!(
            blocker,
            Some("Review fix did not complete cleanly.".to_string())
        );
        assert_eq!(
            review_fix_start_gate(
                &workspace,
                Some(&implementation),
                &AutopilotDecision::RequestChanges,
                Some(&latest_review),
                Some(&failed_review_fix),
            ),
            Err(
                "Review fix already ran after the latest review and did not complete cleanly."
                    .to_string()
            )
        );
    }

    #[test]
    fn next_action_surfaces_review_pass_as_ready_for_app_merge() {
        let implementation = completed_process("Implementation", 0);
        let workspace = workspace();
        let (next_action, blocker) = next_action(
            &workspace,
            Some(&implementation),
            &AutopilotDecision::Pass,
            None,
            None,
            "pr_open_pending_merge",
            None,
        );

        assert_eq!(next_action, AutopilotNextAction::ReadyForMerge);
        assert!(blocker.unwrap().contains("app advance"));
    }

    #[test]
    fn merge_plan_reconciles_merged_pr_to_done_without_new_agent_session() {
        let plan = merge_plan_from_state("merged_pending_done");

        assert_eq!(plan, AutopilotMergePlan::ReconcileDone);
    }

    #[test]
    fn linked_pr_completion_allows_multiple_merged_pull_requests() {
        assert_eq!(
            linked_pr_merge_completion_blocker(&[MergeStatus::Merged, MergeStatus::Merged]),
            None
        );
    }

    #[test]
    fn linked_pr_completion_blocks_when_any_pull_request_remains_open() {
        assert_eq!(
            linked_pr_merge_completion_blocker(&[
                MergeStatus::Merged,
                MergeStatus::Open,
                MergeStatus::Merged
            ]),
            Some(
                "Not all linked pull requests are merged yet: 1 open PR remains after merge attempts."
                    .to_string()
            )
        );
    }

    #[test]
    fn linked_pr_completion_blocks_closed_or_unknown_pull_requests() {
        assert_eq!(
            linked_pr_merge_completion_blocker(&[MergeStatus::Merged, MergeStatus::Closed]),
            Some("Cannot complete autopilot merge: 1 linked PR is closed.".to_string())
        );
        assert_eq!(
            linked_pr_merge_completion_blocker(&[MergeStatus::Unknown]),
            Some("Cannot complete autopilot merge: 1 linked PR has unknown status.".to_string())
        );
    }

    #[test]
    fn linked_pr_completion_reports_missing_prs_as_recoverable() {
        assert_eq!(
            linked_pr_merge_completion_blocker(&[]),
            Some(
                "No linked pull requests were found for this workspace; create or link a PR and rerun app advance."
                    .to_string()
            )
        );
    }

    #[test]
    fn merge_state_requires_all_linked_pull_requests_merged_before_done() {
        assert_eq!(
            merge_state_from_pr_completion(&[MergeStatus::Merged, MergeStatus::Merged]),
            Some("merged_pending_done")
        );
        assert_eq!(
            merge_state_from_pr_completion(&[MergeStatus::Merged, MergeStatus::Closed]),
            Some("blocked_by_pr_requirements")
        );
        assert_eq!(
            merge_state_from_pr_completion(&[MergeStatus::Merged, MergeStatus::Unknown]),
            Some("blocked_by_pr_requirements")
        );
        assert_eq!(merge_state_from_pr_completion(&[]), None);
    }

    #[test]
    fn done_or_archived_state_is_terminal_for_pinned_reconciliation() {
        let implementation = completed_process("Implementation", 0);
        let mut workspace = workspace();
        workspace.pinned = true;
        let (next_action, blocker) = next_action(
            &workspace,
            Some(&implementation),
            &AutopilotDecision::Pass,
            None,
            None,
            "done_or_archived",
            None,
        );

        assert_eq!(next_action, AutopilotNextAction::Done);
        assert_eq!(blocker, None);
    }

    #[test]
    fn next_action_blocks_dirty_or_conflicted_merge_state_after_review_pass() {
        let implementation = completed_process("Implementation", 0);
        let workspace = workspace();
        let (next_action, blocker) = next_action(
            &workspace,
            Some(&implementation),
            &AutopilotDecision::Pass,
            None,
            None,
            "blocked_by_dirty_worktree",
            None,
        );

        assert_eq!(next_action, AutopilotNextAction::InvestigateFailure);
        assert_eq!(
            blocker,
            Some("Merge is blocked by dirty/conflicting workspace changes.".to_string())
        );
        assert_eq!(
            merge_plan_from_state("blocked_by_pr_requirements"),
            AutopilotMergePlan::Blocked(
                "PR merge is blocked by GitHub checks, reviews, conflicts, or mergeability."
                    .to_string()
            )
        );
    }

    #[test]
    fn next_action_blocks_review_pass_on_dirty_pr_without_rerunning_review() {
        let implementation = completed_process("Implementation", 0);
        let workspace = workspace();
        let pr = pr_status(
            AutopilotPrChecksState::Passing,
            Some("PR is not currently mergeable (CONFLICTING/DIRTY)."),
        );

        let (next_action, blocker) = next_action(
            &workspace,
            Some(&implementation),
            &AutopilotDecision::Pass,
            None,
            None,
            "blocked_by_pr_mergeability",
            Some(&pr),
        );

        assert_eq!(next_action, AutopilotNextAction::InvestigateFailure);
        assert!(blocker.unwrap().contains("not currently mergeable"));
    }

    #[test]
    fn next_action_waits_for_pending_checks_after_review_pass() {
        let implementation = completed_process("Implementation", 0);
        let workspace = workspace();
        let pr = pr_status(
            AutopilotPrChecksState::Pending,
            Some("PR checks are not green yet."),
        );

        let (next_action, blocker) = next_action(
            &workspace,
            Some(&implementation),
            &AutopilotDecision::Pass,
            None,
            None,
            "waiting_for_checks",
            Some(&pr),
        );

        assert_eq!(next_action, AutopilotNextAction::MergeWait);
        assert!(blocker.unwrap().contains("not green"));
    }

    #[test]
    fn token_safety_explains_guarded_re_review_without_running_tokens() {
        let implementation = completed_process("Implementation", 0);
        let review = completed_process("Auto review - Codex (medium)", 0);
        let review_fix = completed_process("Review fix - Codex (medium)", 0);

        let (state, note) = token_safety(
            &AutopilotNextAction::StartAutoReview,
            Some("Review fix completed; rerun auto-review."),
            Some(&implementation),
            Some(&review),
            Some(&review_fix),
            &AutopilotDecision::RequestChanges,
        );

        assert_eq!(state, AutopilotTokenSafetyState::Guarded);
        assert!(note.contains("explicit rerun"));
        assert!(note.contains("no agent is running"));
    }

    #[test]
    fn workflow_state_marks_review_pass_as_app_owned_merge_without_token_use() {
        let implementation = completed_process("Implementation", 0);
        let (state, reason) = workflow_state(
            &AutopilotNextAction::ReadyForMerge,
            Some("Review passed; app advance owns merge/done reconciliation."),
            Some(&implementation),
            &AutopilotDecision::Pass,
            None,
            None,
            "pr_open_pending_merge",
        );

        assert_eq!(state, AutopilotWorkflowState::ReviewPassed);
        assert!(reason.contains("app advance"));
    }

    #[test]
    fn batch_selection_stages_all_cards_but_only_runnable_unblocked_cards_start() {
        let blocker = Uuid::new_v4();
        let blocked = Uuid::new_v4();
        let runnable = Uuid::new_v4();
        let queued_by_cap = Uuid::new_v4();
        let candidates = vec![
            BatchCandidate {
                issue_id: blocker,
                workspace_id: Some(Uuid::new_v4()),
                done: false,
                active: true,
                next_action: Some(AutopilotNextAction::WaitForImplementation),
                blockers: vec![],
            },
            BatchCandidate {
                issue_id: blocked,
                workspace_id: None,
                done: false,
                active: false,
                next_action: None,
                blockers: vec![],
            },
            BatchCandidate {
                issue_id: runnable,
                workspace_id: None,
                done: false,
                active: false,
                next_action: None,
                blockers: vec![],
            },
            BatchCandidate {
                issue_id: queued_by_cap,
                workspace_id: None,
                done: false,
                active: false,
                next_action: None,
                blockers: vec![],
            },
        ];

        let plan = plan_batch_advance(&candidates, &[relationship(blocked, blocker)], 2);

        assert_eq!(plan.cards.len(), 4);
        assert_eq!(plan.cards[0].state, BatchCardQueueState::Active);
        assert_eq!(
            plan.cards[1].state,
            BatchCardQueueState::BlockedByDependencies
        );
        assert_eq!(plan.cards[1].blockers, vec![blocker]);
        assert_eq!(plan.cards[2].state, BatchCardQueueState::Runnable);
        assert_eq!(plan.cards[3].state, BatchCardQueueState::Queued);
    }

    #[test]
    fn batch_selection_unblocks_dependents_when_blocker_is_done() {
        let blocker = Uuid::new_v4();
        let dependent = Uuid::new_v4();
        let candidates = vec![
            BatchCandidate {
                issue_id: blocker,
                workspace_id: Some(Uuid::new_v4()),
                done: true,
                active: false,
                next_action: None,
                blockers: vec![],
            },
            BatchCandidate {
                issue_id: dependent,
                workspace_id: None,
                done: false,
                active: false,
                next_action: None,
                blockers: vec![],
            },
        ];

        let plan = plan_batch_advance(&candidates, &[relationship(dependent, blocker)], 1);

        assert_eq!(plan.cards[0].state, BatchCardQueueState::Done);
        assert_eq!(plan.cards[1].state, BatchCardQueueState::Runnable);
        assert!(plan.cards[1].blockers.is_empty());
    }
}
