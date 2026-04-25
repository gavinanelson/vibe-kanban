use std::path::PathBuf;

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
        ExecutorAction, ExecutorActionType, coding_agent_initial::CodingAgentInitialRequest,
        review::{RepoReviewContext as ExecutorRepoReviewContext, ReviewRequest as ReviewAction},
    },
    executors::{BaseCodingAgent, build_review_prompt},
    model_selector::PermissionPolicy,
    profile::ExecutorConfig,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use services::services::container::ContainerService;
use sqlx::FromRow;
use ts_rs::TS;
use utils::{log_msg::LogMsg, response::ApiResponse};
use uuid::Uuid;

use crate::{DeploymentImpl, error::ApiError, middleware::load_workspace_middleware};

const DEFAULT_CODEX_MODEL: &str = "gpt-5.5";
const DEFAULT_CODEX_REASONING: &str = "medium";
const REVIEW_PROMPT: &str = "Review this workspace as an independent Codex reviewer. Do not implement changes. Check the linked GitHub issue acceptance criteria, PR/diff scope, validation evidence, and hygiene. Return one of exactly `Decision: pass` or `Decision: request changes`, followed by blockers, non-blocking notes, validation evidence, and recommended next action.";
const REVIEW_FIX_PROMPT: &str = "Continue the existing implementation by fixing only the blocking issues from the latest auto-review. Use Codex gpt-5.5 with medium reasoning. Keep the change bounded, preserve unrelated dirty work, run focused validation, and do not merge or claim the PR is merged. End with files changed, validation, and whether another auto-review is needed.";

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
    Router::new()
        .route(
            "/workspaces/{id}/implication-autopilot/status",
            get(get_status),
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
        ))
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
async fn start_auto_review(
    Extension(workspace): Extension<Workspace>,
    State(deployment): State<DeploymentImpl>,
    Json(payload): Json<StartAutopilotReviewRequest>,
) -> Result<Json<ApiResponse<AutopilotProcessSummary>>, ApiError> {
    ensure_implication_autopilot_allowed(&workspace, payload.github_repo_full_name.as_deref())?;

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
        decide_from_review_attempts(&deployment, &review_processes).await;
    let pr_merge_state = infer_pr_merge_state(&workspace, &latest_review_decision);

    if let Err(message) = auto_review_start_gate(
        &workspace,
        implementation_process,
        &latest_review_decision,
        review_fix_process,
        &pr_merge_state,
        payload.rerun,
    ) {
        return Err(ApiError::Workspace(WorkspaceError::ValidationError(
            message,
        )));
    }

    let session = Session::create(
        &deployment.db().pool,
        &CreateSession {
            executor: Some("CODEX".to_string()),
            name: Some(auto_review_session_name(payload.rerun)),
        },
        Uuid::new_v4(),
        workspace.id,
    )
    .await?;

    let container_ref = deployment
        .container()
        .ensure_container_exists(&workspace)
        .await?;
    let context = build_autopilot_review_context(&deployment, &workspace, container_ref.as_str())
        .await?;
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
            &workspace,
            &session,
            &action,
            &ExecutionProcessRunReason::CodingAgent,
        )
        .await?;

    Ok(Json(ApiResponse::success(AutopilotProcessSummary {
        id: process.id,
        session_id: process.session_id,
        session_name: session.name,
        status: process.status,
        run_reason: process.run_reason,
        exit_code: process.exit_code,
        started_at: process.started_at.to_rfc3339(),
        completed_at: process
            .completed_at
            .map(|dt: DateTime<Utc>| dt.to_rfc3339()),
    })))
}

#[axum::debug_handler]
async fn start_review_fix(
    Extension(workspace): Extension<Workspace>,
    State(deployment): State<DeploymentImpl>,
    Json(payload): Json<StartAutopilotReviewFixRequest>,
) -> Result<Json<ApiResponse<AutopilotProcessSummary>>, ApiError> {
    ensure_implication_autopilot_allowed(&workspace, payload.github_repo_full_name.as_deref())?;

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
        decide_from_review_attempts(&deployment, &review_processes).await;

    if let Err(message) = review_fix_start_gate(
        &workspace,
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
        .ensure_container_exists(&workspace)
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
            &workspace,
            &session,
            &action,
            &ExecutionProcessRunReason::CodingAgent,
        )
        .await?;

    Ok(Json(ApiResponse::success(AutopilotProcessSummary {
        id: process.id,
        session_id: process.session_id,
        session_name: session.name,
        status: process.status,
        run_reason: process.run_reason,
        exit_code: process.exit_code,
        started_at: process.started_at.to_rfc3339(),
        completed_at: process
            .completed_at
            .map(|dt: DateTime<Utc>| dt.to_rfc3339()),
    })))
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
        if let Ok(base_commit) = deployment.git().get_fork_point(
            &worktree_path,
            &repo.target_branch,
            &workspace.branch,
        ) {
            contexts.push(ExecutorRepoReviewContext {
                repo_id: repo.repo.id,
                repo_name: repo.repo.display_name,
                base_commit,
            });
        }
    }

    Ok((!contexts.is_empty()).then_some(contexts))
}

fn ensure_implication_autopilot_allowed(
    workspace: &Workspace,
    github_repo_full_name: Option<&str>,
) -> Result<(), ApiError> {
    if workspace.task_id.is_none() {
        return Err(ApiError::Workspace(WorkspaceError::ValidationError(
            "Implication autopilot requires a linked board issue.".to_string(),
        )));
    }

    let Some(repo) = github_repo_full_name else {
        return Err(ApiError::Workspace(WorkspaceError::ValidationError(
            "Implication autopilot requires a linked GitHub issue repo.".to_string(),
        )));
    };

    if repo.trim().eq_ignore_ascii_case("gavinanelson/implication") {
        return Ok(());
    }

    Err(ApiError::Workspace(WorkspaceError::ValidationError(
        "Implication autopilot is only enabled for gavinanelson/implication issues.".to_string(),
    )))
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

    Ok(ImplicationAutopilotStatus {
        workspace_id: workspace.id,
        workspace_name: workspace.name.clone(),
        implementation_state,
        auto_review_state,
        latest_review_decision,
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
        daemonized: false,
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
    if reviews.is_empty() {
        return (AutopilotDecision::Missing, None, None);
    }
    if let Some(row) = running_review_attempt(reviews) {
        return (AutopilotDecision::Running, None, Some(row));
    }

    for row in reviews {
        if row.status != ExecutionProcessStatus::Completed || row.exit_code != Some(0) {
            continue;
        }
        let text = process_agent_text(deployment, row.id).await;
        if text.trim().is_empty() {
            continue;
        }
        let decision = decision_from_text(&text);
        return (decision, Some(excerpt(&text)), Some(*row));
    }

    let latest = reviews[0];
    if latest.status == ExecutionProcessStatus::Killed
        || latest.status == ExecutionProcessStatus::Completed
    {
        return (AutopilotDecision::Missing, None, Some(latest));
    }
    (AutopilotDecision::Failed, None, Some(latest))
}

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
        assert_eq!(auto_review_session_name(false), "Auto review - Codex (medium)");
        assert_eq!(
            auto_review_session_name(true),
            "Auto review rerun - Codex (medium)"
        );
        assert_eq!(review_fix_session_name(), "Review fix - Codex (medium)");
        assert!(is_review_fix_session(Some("Review fix - Codex (medium)")));
        assert!(!is_auto_review_session(Some("Review fix - Codex (medium)")));
        assert!(!review_fix_session_name()
            .to_ascii_lowercase()
            .starts_with("auto review"));
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
        let mut workspace = workspace();
        assert!(
            ensure_implication_autopilot_allowed(&workspace, Some("gavinanelson/implication"))
                .is_ok()
        );

        assert!(
            ensure_implication_autopilot_allowed(&workspace, Some("gavinanelson/vibe-kanban"))
                .is_err()
        );

        workspace.task_id = None;
        assert!(
            ensure_implication_autopilot_allowed(&workspace, Some("gavinanelson/implication"))
                .is_err()
        );
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
}
