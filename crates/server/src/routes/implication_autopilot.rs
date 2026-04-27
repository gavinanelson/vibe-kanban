use std::{
    collections::HashSet,
    future::Future,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use api_types::{Issue, UpdateIssueRequest};
use axum::{
    Extension, Json, Router,
    extract::State,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use db::models::{
    execution_process::{ExecutionProcessRunReason, ExecutionProcessStatus},
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
use serde::{Deserialize, Serialize};
use serde_json::Value;
use services::services::{container::ContainerService, remote_client::RemoteClient};
use sqlx::FromRow;
use ts_rs::TS;
use utils::{log_msg::LogMsg, response::ApiResponse};
use uuid::Uuid;

use crate::{DeploymentImpl, error::ApiError, middleware::load_workspace_middleware};

const DEFAULT_CODEX_MODEL: &str = "gpt-5.5";
const DEFAULT_CODEX_REASONING: &str = "medium";
const REVIEW_PROMPT: &str = "Review this workspace as an independent Codex reviewer. Do not implement changes. Check the linked GitHub issue acceptance criteria, PR/diff scope, validation evidence, and hygiene. Return one of exactly `Decision: pass` or `Decision: request changes`, followed by blockers, non-blocking notes, validation evidence, and recommended next action.";
const REVIEW_FIX_PROMPT: &str = "Continue the existing implementation by fixing only the blocking issues from the latest auto-review. Use Codex gpt-5.5 with medium reasoning. Keep the change bounded, preserve unrelated dirty work, run focused validation, and do not merge or claim the PR is merged. End with files changed, validation, and whether another auto-review is needed.";

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
    MergeHandoff,
    Blocked,
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

    let action_taken = advance_workspace_once(&deployment, &workspace).await?;
    let status = build_status(&deployment, &workspace).await?;
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
    let (latest_review_decision, _, _) =
        decide_from_review_attempts(deployment, &review_processes).await;
    let pr_merge_state = infer_pr_merge_state(workspace, &latest_review_decision);

    if let Err(message) = auto_review_start_gate(
        workspace,
        implementation_process,
        &latest_review_decision,
        review_fix_process,
        &pr_merge_state,
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
    let (latest_review_decision, latest_review_excerpt, _) =
        decide_from_review_attempts(deployment, &review_processes).await;

    if let Err(message) = review_fix_start_gate(
        workspace,
        implementation_process,
        &latest_review_decision,
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
) -> Result<AutopilotAdvanceAction, ApiError> {
    let Some(_advance_guard) = WorkspaceAdvanceGuard::try_acquire(workspace.id) else {
        return Ok(AutopilotAdvanceAction::Noop);
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
            let rerun = status.latest_review_decision == AutopilotDecision::RequestChanges
                && status.review_fix_state == "completed";
            start_auto_review_for_workspace(deployment, workspace, rerun).await?;
            if let Some(issue) = issue.as_ref() {
                promote_issue_to_in_review(&client, issue).await?;
            }
            Ok(AutopilotAdvanceAction::StartedAutoReview)
        }
        AutopilotNextAction::StartReviewFix => {
            start_review_fix_for_workspace(deployment, workspace).await?;
            Ok(AutopilotAdvanceAction::StartedReviewFix)
        }
        AutopilotNextAction::ReadyForMerge => Ok(AutopilotAdvanceAction::MergeHandoff),
        AutopilotNextAction::WaitForImplementation => Ok(AutopilotAdvanceAction::Noop),
        AutopilotNextAction::NoWorkspace | AutopilotNextAction::InvestigateFailure => {
            Ok(AutopilotAdvanceAction::Blocked)
        }
        AutopilotNextAction::WaitForAutoReview
        | AutopilotNextAction::WaitForReviewFix
        | AutopilotNextAction::MergeWait
        | AutopilotNextAction::Done => Ok(AutopilotAdvanceAction::Noop),
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
    let pr_merge_state = infer_pr_merge_state(workspace, &latest_review_decision);
    let (next_action, blocker) = next_action(
        workspace,
        implementation_process,
        &latest_review_decision,
        review_fix_process,
        &pr_merge_state,
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
    process_summary_from_parts(
        row.id,
        row.session_id,
        row.session_name.clone(),
        row.status.clone(),
        row.run_reason.clone(),
        row.exit_code,
        row.started_at.clone(),
        row.completed_at.clone(),
    )
}

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

fn infer_pr_merge_state(workspace: &Workspace, decision: &AutopilotDecision) -> String {
    if workspace.archived || workspace.worktree_deleted {
        return "done_or_archived".to_string();
    }
    match decision {
        AutopilotDecision::Pass => "review_passed_merge_status_unknown".to_string(),
        AutopilotDecision::RequestChanges => "blocked_by_review".to_string(),
        AutopilotDecision::Running => "waiting_for_review".to_string(),
        AutopilotDecision::Missing => "waiting_for_review".to_string(),
        AutopilotDecision::Failed => "review_failed".to_string(),
    }
}

fn review_fix_start_gate(
    workspace: &Workspace,
    implementation: Option<&ProcessRow>,
    decision: &AutopilotDecision,
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
        if row.status == ExecutionProcessStatus::Completed && row.exit_code == Some(0) {
            return Err("Review fix already completed; rerun auto-review next.".to_string());
        }
    }
    Ok(())
}

fn auto_review_start_gate(
    workspace: &Workspace,
    implementation: Option<&ProcessRow>,
    decision: &AutopilotDecision,
    review_fix: Option<&ProcessRow>,
    pr_merge_state: &str,
    rerun: bool,
) -> Result<(), String> {
    let (action, blocker) = next_action(
        workspace,
        implementation,
        decision,
        review_fix,
        pr_merge_state,
    );

    if action != AutopilotNextAction::StartAutoReview {
        return Err(format!(
            "Auto-review cannot start while the next action is {}.",
            next_action_name(&action)
        ));
    }

    if decision == &AutopilotDecision::RequestChanges {
        let review_fix_completed = review_fix.is_some_and(|row| {
            row.status == ExecutionProcessStatus::Completed && row.exit_code == Some(0)
        });
        if review_fix_completed && !rerun {
            return Err(
                "Auto-review rerun must be explicit after review fix completes.".to_string(),
            );
        }
    }

    if let Some(blocker) = blocker {
        if !rerun {
            return Err(blocker);
        }
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
                && review_fix.is_some_and(|row| {
                    row.status == ExecutionProcessStatus::Completed && row.exit_code == Some(0)
                })
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
                .unwrap_or("Review passed; merge handoff is visible.")
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

fn next_action(
    workspace: &Workspace,
    implementation: Option<&ProcessRow>,
    decision: &AutopilotDecision,
    review_fix: Option<&ProcessRow>,
    pr_merge_state: &str,
) -> (AutopilotNextAction, Option<String>) {
    if workspace.archived || workspace.worktree_deleted || pr_merge_state == "done_or_archived" {
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
            Some(row)
                if row.status == ExecutionProcessStatus::Completed && row.exit_code == Some(0) =>
            {
                (
                    AutopilotNextAction::StartAutoReview,
                    Some("Review fix completed; rerun auto-review.".to_string()),
                )
            }
            _ => (AutopilotNextAction::StartReviewFix, None),
        },
        // TODO(implication-autopilot): wire a safe PR merge helper here when the
        // app can prove the PR is mergeable and checks/review requirements pass.
        // The existing direct workspace merge helper intentionally rejects open
        // PRs, so this state must remain an honest operator handoff for now.
        AutopilotDecision::Pass => (
            AutopilotNextAction::ReadyForMerge,
            Some(
                "Review passed; PR/check mergeability is not daemonized in this UI slice yet."
                    .to_string(),
            ),
        ),
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

    fn relationship(issue_id: Uuid, blocker: Uuid) -> BatchAdvanceRelationship {
        BatchAdvanceRelationship {
            issue_id,
            blocking_issue_id: blocker,
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
                "waiting_for_review",
                false,
            ),
            Ok(())
        );
    }

    #[test]
    fn action_gating_requires_explicit_rerun_after_review_fix_completes() {
        let implementation = completed_process("Implementation", 0);
        let review_fix = completed_process("Review fix", 0);
        let workspace = workspace();

        assert_eq!(
            auto_review_start_gate(
                &workspace,
                Some(&implementation),
                &AutopilotDecision::RequestChanges,
                Some(&review_fix),
                "blocked_by_review",
                false,
            ),
            Err("Auto-review rerun must be explicit after review fix completes.".to_string())
        );

        assert_eq!(
            auto_review_start_gate(
                &workspace,
                Some(&implementation),
                &AutopilotDecision::RequestChanges,
                Some(&review_fix),
                "blocked_by_review",
                true,
            ),
            Ok(())
        );
    }

    #[test]
    fn action_gating_blocks_auto_review_when_review_fix_is_next() {
        let implementation = completed_process("Implementation", 0);
        let workspace = workspace();

        assert_eq!(
            auto_review_start_gate(
                &workspace,
                Some(&implementation),
                &AutopilotDecision::RequestChanges,
                None,
                "blocked_by_review",
                false,
            ),
            Err("Auto-review cannot start while the next action is start_review_fix.".to_string())
        );
    }

    #[test]
    fn next_action_surfaces_review_pass_as_ready_for_merge_but_not_automated() {
        let implementation = completed_process("Implementation", 0);
        let workspace = workspace();
        let (next_action, blocker) = next_action(
            &workspace,
            Some(&implementation),
            &AutopilotDecision::Pass,
            None,
            "review_passed_merge_status_unknown",
        );

        assert_eq!(next_action, AutopilotNextAction::ReadyForMerge);
        assert!(blocker.unwrap().contains("not daemonized"));
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
    fn workflow_state_marks_review_pass_as_merge_handoff_without_token_use() {
        let implementation = completed_process("Implementation", 0);
        let (state, reason) = workflow_state(
            &AutopilotNextAction::ReadyForMerge,
            Some("Review passed; merge handoff is visible."),
            Some(&implementation),
            &AutopilotDecision::Pass,
            None,
            "review_passed_merge_status_unknown",
        );

        assert_eq!(state, AutopilotWorkflowState::ReviewPassed);
        assert!(reason.contains("merge handoff"));
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
