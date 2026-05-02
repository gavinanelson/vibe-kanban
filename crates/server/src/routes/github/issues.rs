use axum::{
    Json, Router,
    extract::{Path, Query, State},
    response::Json as ResponseJson,
    routing::{get, post},
};
use git_host::{
    GitHostError,
    github::{GhCli, GhCliError, GitHubIssueComment, GitHubIssueSummary},
};
use serde::{Deserialize, Serialize};
use tokio::task;
use url::Url;
use utils::response::ApiResponse;

use crate::{DeploymentImpl, error::ApiError};

#[derive(Debug, Deserialize)]
pub struct SearchGitHubIssuesQuery {
    pub repo: String,
    pub q: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct ResolveGitHubIssueUrlRequest {
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct GitHubIssuePathParams {
    pub owner: String,
    pub repo: String,
    pub number: i64,
}

#[derive(Debug, Serialize)]
pub struct SearchGitHubIssuesResponse {
    pub issues: Vec<GitHubIssueSummary>,
}

#[derive(Debug, Deserialize)]
pub struct ListGitHubIssueCommentsQuery {
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ListGitHubIssueCommentsResponse {
    pub comments: Vec<GitHubIssueComment>,
}

#[derive(Debug, Deserialize)]
pub struct CreateGitHubIssueCommentRequest {
    pub body: String,
}

pub fn router() -> Router<DeploymentImpl> {
    Router::new()
        .route("/github/issues/search", get(search_github_issues))
        .route("/github/issues/resolve-url", post(resolve_github_issue_url))
        .route(
            "/github/issues/{owner}/{repo}/{number}",
            get(get_github_issue),
        )
        .route(
            "/github/issues/{owner}/{repo}/{number}/comments",
            get(list_github_issue_comments).post(create_github_issue_comment),
        )
}

pub async fn search_github_issues(
    State(_deployment): State<DeploymentImpl>,
    Query(query): Query<SearchGitHubIssuesQuery>,
) -> Result<ResponseJson<ApiResponse<SearchGitHubIssuesResponse>>, ApiError> {
    let repo_full_name = normalize_repo_full_name(&query.repo).map_err(ApiError::BadRequest)?;
    let search_query = query.q.unwrap_or_default();
    let limit = query.limit.unwrap_or(20).clamp(1, 50);

    let issues =
        run_gh(move |cli| cli.search_issues(&repo_full_name, &search_query, limit)).await?;

    Ok(ResponseJson(ApiResponse::success(
        SearchGitHubIssuesResponse { issues },
    )))
}

pub async fn resolve_github_issue_url(
    State(_deployment): State<DeploymentImpl>,
    Json(payload): Json<ResolveGitHubIssueUrlRequest>,
) -> Result<ResponseJson<ApiResponse<GitHubIssueSummary>>, ApiError> {
    let (repo_full_name, issue_number) =
        parse_issue_url(&payload.url).map_err(ApiError::BadRequest)?;
    let issue = run_gh(move |cli| cli.get_issue(&repo_full_name, issue_number)).await?;

    Ok(ResponseJson(ApiResponse::success(issue)))
}

pub async fn get_github_issue(
    State(_deployment): State<DeploymentImpl>,
    Path(params): Path<GitHubIssuePathParams>,
) -> Result<ResponseJson<ApiResponse<GitHubIssueSummary>>, ApiError> {
    let repo_full_name = normalize_repo_full_name(&format!("{}/{}", params.owner, params.repo))
        .map_err(ApiError::BadRequest)?;
    let issue = run_gh(move |cli| cli.get_issue(&repo_full_name, params.number)).await?;

    Ok(ResponseJson(ApiResponse::success(issue)))
}

pub async fn list_github_issue_comments(
    State(_deployment): State<DeploymentImpl>,
    Path(params): Path<GitHubIssuePathParams>,
    Query(query): Query<ListGitHubIssueCommentsQuery>,
) -> Result<ResponseJson<ApiResponse<ListGitHubIssueCommentsResponse>>, ApiError> {
    let repo_full_name = normalize_repo_full_name(&format!("{}/{}", params.owner, params.repo))
        .map_err(ApiError::BadRequest)?;
    let issue_number = params.number;
    let limit = query.limit.unwrap_or(10).clamp(1, 25);
    let comments =
        run_gh(move |cli| cli.get_issue_comments(&repo_full_name, issue_number, limit)).await?;

    Ok(ResponseJson(ApiResponse::success(
        ListGitHubIssueCommentsResponse { comments },
    )))
}

pub async fn create_github_issue_comment(
    State(_deployment): State<DeploymentImpl>,
    Path(params): Path<GitHubIssuePathParams>,
    Json(payload): Json<CreateGitHubIssueCommentRequest>,
) -> Result<ResponseJson<ApiResponse<()>>, ApiError> {
    let repo_full_name = normalize_repo_full_name(&format!("{}/{}", params.owner, params.repo))
        .map_err(ApiError::BadRequest)?;
    let issue_number = params.number;
    let body = format_kanban_update_comment(&payload.body).map_err(ApiError::BadRequest)?;
    run_gh(move |cli| cli.create_issue_comment(&repo_full_name, issue_number, &body)).await?;

    Ok(ResponseJson(ApiResponse::success(())))
}

async fn run_gh<T, F>(f: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(GhCli) -> Result<T, GhCliError> + Send + 'static,
{
    task::spawn_blocking(move || {
        let cli = GhCli::new();
        f(cli).map_err(|err| {
            let git_host_error: GitHostError = err.into();
            ApiError::from(git_host_error)
        })
    })
    .await
    .map_err(|err| ApiError::BadGateway(format!("GitHub CLI task failed: {err}")))?
}

fn normalize_repo_full_name(repo: &str) -> Result<String, String> {
    let normalized = repo
        .trim()
        .trim_matches('/')
        .trim_end_matches(".git")
        .to_string();

    let mut parts = normalized.split('/');
    let owner = parts.next().unwrap_or_default();
    let repo_name = parts.next().unwrap_or_default();

    if owner.is_empty() || repo_name.is_empty() || parts.next().is_some() {
        return Err("Repository must be in owner/repo format".to_string());
    }

    Ok(format!("{owner}/{repo_name}"))
}

fn parse_issue_url(url: &str) -> Result<(String, i64), String> {
    let parsed = Url::parse(url).map_err(|_| "Invalid GitHub issue URL".to_string())?;

    match parsed.host_str() {
        Some("github.com") | Some("www.github.com") => {}
        _ => {
            return Err("Only github.com issue URLs are supported".to_string());
        }
    }

    let segments: Vec<_> = parsed.path_segments().into_iter().flatten().collect();
    if segments.len() < 4 || segments[2] != "issues" {
        return Err("URL must point to a GitHub issue".to_string());
    }

    let repo_full_name = normalize_repo_full_name(&format!("{}/{}", segments[0], segments[1]))?;
    let issue_number = segments[3]
        .parse::<i64>()
        .map_err(|_| "GitHub issue URL must include a numeric issue number".to_string())?;

    if issue_number <= 0 {
        return Err("GitHub issue number must be positive".to_string());
    }

    Ok((repo_full_name, issue_number))
}

fn format_kanban_update_comment(body: &str) -> Result<String, String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err("Comment body must not be empty".to_string());
    }

    if trimmed.to_ascii_lowercase().starts_with("kanban update:") {
        Ok(trimmed.to_string())
    } else {
        Ok(format!("Kanban update: {trimmed}"))
    }
}

#[cfg(test)]
mod tests {
    use super::format_kanban_update_comment;

    #[test]
    fn kanban_update_comment_prefix_is_enforced() {
        assert_eq!(
            format_kanban_update_comment("ready for review").unwrap(),
            "Kanban update: ready for review"
        );
        assert_eq!(
            format_kanban_update_comment("Kanban update: done with evidence").unwrap(),
            "Kanban update: done with evidence"
        );
        assert!(format_kanban_update_comment("   ").is_err());
    }
}
