import type { JsonValue } from 'shared/remote-types';

type JsonRecord = { [key: string]: JsonValue };

export type GitHubIssueLink = {
  repo_full_name: string;
  issue_number: number;
  issue_url: string;
  last_seen_title?: string | null;
  last_seen_state?: string | null;
  last_seen_updated_at?: string | null;
  latest_pr_url?: string | null;
  latest_pr_number?: number | null;
  latest_agent_checkpoint?: string | null;
  latest_agent_checkpoint_at?: string | null;
};

export type GitHubIssueSummary = {
  repo_full_name: string;
  issue_number: number;
  issue_url: string;
  title: string;
  state: string;
  updated_at?: string | null;
};

const GITHUB_LINK_KEY = 'github_link';

function isJsonRecord(value: unknown): value is JsonRecord {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function normalizeRepoFullName(repoFullName: string): string {
  return repoFullName
    .trim()
    .replace(/^\/+|\/+$/g, '')
    .replace(/\.git$/i, '');
}

function normalizeIssueUrl(issueUrl: string): string {
  const trimmed = issueUrl.trim();

  try {
    const parsed = new URL(trimmed);
    parsed.hash = '';
    parsed.search = '';
    return parsed.toString().replace(/\/+$/, '');
  } catch {
    return trimmed.replace(/\/+$/, '');
  }
}

function normalizeGitHubIssueLink(
  link: GitHubIssueLink
): GitHubIssueLink | null {
  const repoFullName = normalizeRepoFullName(link.repo_full_name);
  const issueUrl = normalizeIssueUrl(link.issue_url);
  const issueNumber = Number(link.issue_number);

  if (!repoFullName || !Number.isInteger(issueNumber) || issueNumber <= 0) {
    return null;
  }

  if (!issueUrl) {
    return null;
  }

  return {
    ...link,
    repo_full_name: repoFullName,
    issue_number: issueNumber,
    issue_url: issueUrl,
  };
}

export function getGitHubIssueLink(
  extensionMetadata: unknown
): GitHubIssueLink | null {
  if (!isJsonRecord(extensionMetadata)) {
    return null;
  }

  const candidate = extensionMetadata[GITHUB_LINK_KEY];

  if (!isJsonRecord(candidate)) {
    return null;
  }

  return normalizeGitHubIssueLink(candidate as GitHubIssueLink);
}

export function setGitHubIssueLink(
  extensionMetadata: unknown,
  link: GitHubIssueLink
): JsonValue {
  const normalizedLink = normalizeGitHubIssueLink(link);

  if (!normalizedLink) {
    throw new Error('Invalid GitHub issue link payload');
  }

  const nextMetadata = isJsonRecord(extensionMetadata)
    ? { ...extensionMetadata }
    : {};

  nextMetadata[GITHUB_LINK_KEY] = normalizedLink;
  return nextMetadata;
}

export function clearGitHubIssueLink(
  extensionMetadata: unknown
): JsonRecord | null {
  if (!isJsonRecord(extensionMetadata)) {
    return null;
  }

  const nextMetadata = { ...extensionMetadata };
  delete nextMetadata[GITHUB_LINK_KEY];

  return Object.keys(nextMetadata).length > 0 ? nextMetadata : null;
}

export function parseGitHubIssueUrl(url: string): GitHubIssueLink | null {
  const trimmed = url.trim();

  if (!trimmed) {
    return null;
  }

  let parsed: URL;
  try {
    parsed = new URL(trimmed);
  } catch {
    return null;
  }

  const segments = parsed.pathname.split('/').filter(Boolean);
  if (segments.length < 4) {
    return null;
  }

  const [owner, repo, resource, issueNumberRaw] = segments;
  if (!owner || !repo || resource !== 'issues') {
    return null;
  }

  const issueNumber = Number.parseInt(issueNumberRaw, 10);
  if (!Number.isInteger(issueNumber) || issueNumber <= 0) {
    return null;
  }

  return {
    repo_full_name: normalizeRepoFullName(`${owner}/${repo}`),
    issue_number: issueNumber,
    issue_url: normalizeIssueUrl(trimmed),
  };
}

export function createGitHubIssueLinkFromSummary(
  summary: GitHubIssueSummary
): GitHubIssueLink {
  return {
    repo_full_name: normalizeRepoFullName(summary.repo_full_name),
    issue_number: summary.issue_number,
    issue_url: normalizeIssueUrl(summary.issue_url),
    last_seen_title: summary.title,
    last_seen_state: summary.state,
    last_seen_updated_at: summary.updated_at ?? null,
  };
}
