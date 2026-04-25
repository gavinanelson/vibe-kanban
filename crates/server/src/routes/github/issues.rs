use axum::{
    Json, Router,
    extract::{Path, Query, State},
    response::Json as ResponseJson,
    routing::{get, post},
};
use git_host::{
    GitHostError,
    github::{GhCli, GhCliError, GitHubIssueSummary},
};
use serde::Deserialize;
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

#[derive(Debug, serde::Serialize)]
pub struct SearchGitHubIssuesResponse {
    pub issues: Vec<GitHubIssueSummary>,
}

pub fn router() -> Router<DeploymentImpl> {
    Router::new()
        .route("/github/issues/search", get(search_github_issues))
        .route("/github/issues/resolve-url", post(resolve_github_issue_url))
        .route(
            "/github/issues/{owner}/{repo}/{number}",
            get(get_github_issue),
        )
}

pub async fn search_github_issues(
    State(_deployment): State<DeploymentImpl>,
    Query(query): Query<SearchGitHubIssuesQuery>,
) -> Result<ResponseJson<ApiResponse<SearchGitHubIssuesResponse>>, ApiError> {
    let repo_full_name = normalize_repo_full_name(&query.repo)?;
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
    let (repo_full_name, issue_number) = parse_issue_url(&payload.url)?;
    let issue = run_gh(move |cli| cli.get_issue(&repo_full_name, issue_number)).await?;

    Ok(ResponseJson(ApiResponse::success(issue)))
}

pub async fn get_github_issue(
    State(_deployment): State<DeploymentImpl>,
    Path(params): Path<GitHubIssuePathParams>,
) -> Result<ResponseJson<ApiResponse<GitHubIssueSummary>>, ApiError> {
    let repo_full_name = normalize_repo_full_name(&format!("{}/{}", params.owner, params.repo))?;
    let issue = run_gh(move |cli| cli.get_issue(&repo_full_name, params.number)).await?;

    Ok(ResponseJson(ApiResponse::success(issue)))
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

fn normalize_repo_full_name(repo: &str) -> Result<String, ApiError> {
    let normalized = repo
        .trim()
        .trim_matches('/')
        .trim_end_matches(".git")
        .to_string();

    let mut parts = normalized.split('/');
    let owner = parts.next().unwrap_or_default();
    let repo_name = parts.next().unwrap_or_default();

    if owner.is_empty() || repo_name.is_empty() || parts.next().is_some() {
        return Err(ApiError::BadRequest(
            "Repository must be in owner/repo format".to_string(),
        ));
    }

    Ok(format!("{owner}/{repo_name}"))
}

fn parse_issue_url(url: &str) -> Result<(String, i64), ApiError> {
    let parsed = Url::parse(url)
        .map_err(|_| ApiError::BadRequest("Invalid GitHub issue URL".to_string()))?;

    match parsed.host_str() {
        Some("github.com") | Some("www.github.com") => {}
        _ => {
            return Err(ApiError::BadRequest(
                "Only github.com issue URLs are supported".to_string(),
            ));
        }
    }

    let segments: Vec<_> = parsed.path_segments().into_iter().flatten().collect();
    if segments.len() < 4 || segments[2] != "issues" {
        return Err(ApiError::BadRequest(
            "URL must point to a GitHub issue".to_string(),
        ));
    }

    let repo_full_name = normalize_repo_full_name(&format!("{}/{}", segments[0], segments[1]))?;
    let issue_number = segments[3].parse::<i64>().map_err(|_| {
        ApiError::BadRequest("GitHub issue URL must include a numeric issue number".to_string())
    })?;

    if issue_number <= 0 {
        return Err(ApiError::BadRequest(
            "GitHub issue number must be positive".to_string(),
        ));
    }

    Ok((repo_full_name, issue_number))
}
