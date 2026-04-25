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
};
use deployment::Deployment;
use executors::{
    actions::{ExecutorAction, ExecutorActionType, review::ReviewRequest as ReviewAction},
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

    let session = Session::create(
        &deployment.db().pool,
        &CreateSession {
            executor: Some("CODEX".to_string()),
            name: Some(format!(
                "Auto review{} - Codex ({})",
                if payload.rerun { " rerun" } else { "" },
                DEFAULT_CODEX_REASONING
            )),
        },
        Uuid::new_v4(),
        workspace.id,
    )
    .await?;

    let container_ref = deployment
        .container()
        .ensure_container_exists(&workspace)
        .await?;
    let _ = container_ref;
    let executor_config = ExecutorConfig {
        executor: BaseCodingAgent::Codex,
        variant: None,
        model_id: Some(DEFAULT_CODEX_MODEL.to_string()),
        agent_id: None,
        reasoning_id: Some(DEFAULT_CODEX_REASONING.to_string()),
        permission_policy: Some(PermissionPolicy::Auto),
    };
    let prompt = build_review_prompt(None, Some(REVIEW_PROMPT));
    let action = ExecutorAction::new(
        ExecutorActionType::ReviewRequest(ReviewAction {
            executor_config,
            context: None,
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
    name.unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .starts_with("auto review")
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
    if reviews
        .iter()
        .any(|row| row.status == ExecutionProcessStatus::Running)
    {
        return (AutopilotDecision::Running, None, None);
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
}
