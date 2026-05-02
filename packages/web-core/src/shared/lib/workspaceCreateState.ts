import type { CreateModeInitialState } from '@/shared/types/createMode';
import type { DraftWorkspaceData } from 'shared/types';
import { ScratchType } from 'shared/types';
import type { AppRuntime } from '@/shared/hooks/useAppRuntime';
import { scratchApi } from '@/shared/lib/api';
import { localStorageScratchUpdate } from '@/shared/hooks/useLocalStorageScratch';
import type {
  GitHubIssueComment,
  GitHubIssueLink,
} from '@/shared/lib/githubIssueLink';

interface WorkspaceDefaultsLike {
  preferredRepos?: CreateModeInitialState['preferredRepos'];
  project_id?: string | null;
}

interface LocalWorkspaceLike {
  id: string;
}

interface LinkedIssueSource {
  id: string;
  simple_id: string;
  title: string;
}

export const DEFAULT_WORKSPACE_CREATE_DRAFT_ID =
  '00000000-0000-0000-0000-000000000001';

export function buildWorkspaceCreatePrompt(
  title: string | null | undefined,
  description: string | null | undefined,
  githubIssueLink?: GitHubIssueLink | null,
  githubIssueComments: GitHubIssueComment[] = []
): string | null {
  const trimmedTitle = title?.trim();
  if (!trimmedTitle) return null;

  const trimmedDescription = description?.trim();
  const basePrompt = trimmedDescription
    ? `${trimmedTitle}\n\n${trimmedDescription}`
    : trimmedTitle;

  if (!githubIssueLink) {
    return basePrompt;
  }

  const commentContext = formatGitHubIssueCommentContext(githubIssueComments);
  const commentSection = commentContext
    ? `\n\nRecent GitHub issue comments:\n${commentContext}`
    : '';

  return `${basePrompt}\n\n---\nLinked GitHub issue: ${githubIssueLink.repo_full_name}#${githubIssueLink.issue_number}\nIssue URL: ${githubIssueLink.issue_url}${commentSection}\nMaintain task progress visibility on that issue while you work.`;
}

export function formatGitHubIssueCommentContext(
  comments: GitHubIssueComment[],
  limit = 5
): string {
  return comments
    .slice(0, limit)
    .filter((comment) => comment.body.trim().length > 0)
    .map((comment) => {
      const author = comment.author_login
        ? `@${comment.author_login}`
        : 'unknown author';
      const createdAt = comment.created_at ?? 'unknown time';
      const body = truncateGitHubIssueComment(
        comment.body.trim().replace(/\n{3,}/g, '\n\n')
      );
      return `- ${author} at ${createdAt}: ${body}`;
    })
    .filter((line) => line.trim().length > 0)
    .join('\n');
}

function truncateGitHubIssueComment(body: string): string {
  const maxLength = 1200;
  if (body.length <= maxLength) {
    return body;
  }
  return `${body.slice(0, maxLength).trimEnd()}\n[truncated]`;
}

export function buildLinkedIssueCreateState(
  issue: LinkedIssueSource | null | undefined,
  projectId: string
): NonNullable<CreateModeInitialState['linkedIssue']> | null {
  if (!issue) return null;
  return {
    issueId: issue.id,
    simpleId: issue.simple_id,
    title: issue.title,
    remoteProjectId: projectId,
  };
}

export function buildWorkspaceCreateInitialState(args: {
  prompt: string | null;
  defaults?: WorkspaceDefaultsLike | null;
  linkedIssue?: CreateModeInitialState['linkedIssue'];
  executorConfig?: CreateModeInitialState['executorConfig'];
}): CreateModeInitialState {
  return {
    initialPrompt: args.prompt,
    preferredRepos: args.defaults?.preferredRepos ?? null,
    project_id: args.defaults?.project_id ?? null,
    linkedIssue: args.linkedIssue ?? null,
    executorConfig: args.executorConfig ?? null,
  };
}

export function buildLocalWorkspaceIdSet(
  activeWorkspaces: LocalWorkspaceLike[],
  archivedWorkspaces: LocalWorkspaceLike[]
): Set<string> {
  return new Set([
    ...activeWorkspaces.map((workspace) => workspace.id),
    ...archivedWorkspaces.map((workspace) => workspace.id),
  ]);
}

export function toDraftWorkspaceData(
  initialState: CreateModeInitialState
): DraftWorkspaceData {
  return {
    message: initialState.initialPrompt ?? '',
    repos:
      initialState.preferredRepos?.map((repo) => ({
        repo_id: repo.repo_id,
        target_branch: repo.target_branch ?? '',
      })) ?? [],
    executor_config: initialState.executorConfig ?? null,
    linked_issue: initialState.linkedIssue
      ? {
          issue_id: initialState.linkedIssue.issueId,
          simple_id: initialState.linkedIssue.simpleId ?? '',
          title: initialState.linkedIssue.title ?? '',
          remote_project_id: initialState.linkedIssue.remoteProjectId,
        }
      : null,
    attachments: [],
  };
}

export async function persistWorkspaceCreateDraft(
  initialState: CreateModeInitialState,
  draftId = DEFAULT_WORKSPACE_CREATE_DRAFT_ID,
  runtime: AppRuntime = 'local'
): Promise<string | null> {
  const draftData = toDraftWorkspaceData(initialState);
  const payload = {
    type: 'DRAFT_WORKSPACE' as const,
    data: draftData,
  };

  try {
    if (runtime === 'remote') {
      const didPersist = localStorageScratchUpdate(
        ScratchType.DRAFT_WORKSPACE,
        draftId,
        {
          payload,
        }
      );
      if (!didPersist) {
        throw new Error('Failed to persist create-workspace draft in storage');
      }
    } else {
      await scratchApi.update(ScratchType.DRAFT_WORKSPACE, draftId, {
        payload,
      });
    }
    return draftId;
  } catch (error) {
    console.error('Failed to persist create-workspace draft:', error);
    return null;
  }
}
