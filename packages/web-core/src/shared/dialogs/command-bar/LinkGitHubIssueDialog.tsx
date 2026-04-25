import { useEffect, useMemo, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { ArrowSquareOut, LinkIcon, XIcon } from '@phosphor-icons/react';
import { create, useModal } from '@ebay/nice-modal-react';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@vibe/ui/components/KeyboardDialog';
import { Button } from '@vibe/ui/components/Button';
import { Input } from '@vibe/ui/components/Input';
import { Label } from '@vibe/ui/components/Label';
import { defineModal } from '@/shared/lib/modals';
import { githubIssuesApi } from '@/shared/lib/api';
import {
  clearGitHubIssueLink,
  createGitHubIssueLinkFromSummary,
  getGitHubIssueLink,
  setGitHubIssueLink,
  type GitHubIssueSummary,
  type GitHubIssueLink,
} from '@/shared/lib/githubIssueLink';
import { ProjectProvider } from '@/shared/providers/remote/ProjectProvider';
import { useProjectContext } from '@/shared/hooks/useProjectContext';
import { FALLBACK_GITHUB_REPO } from '@/shared/lib/projectGitHubDefaults';

export interface LinkGitHubIssueDialogProps {
  projectId: string;
  issueId?: string;
  hostId?: string | null;
  initialRepo?: string | null;
  initialLink?: GitHubIssueLink | null;
}

export type LinkGitHubIssueDialogResult =
  | { action: 'linked'; link: GitHubIssueLink }
  | { action: 'unlinked' }
  | undefined;

function LinkGitHubIssueContent({
  issueId,
  hostId,
  initialRepo,
  initialLink,
}: Omit<LinkGitHubIssueDialogProps, 'projectId'>) {
  const modal = useModal();
  const { getIssue, updateIssue } = useProjectContext();

  const currentIssue = issueId ? getIssue(issueId) : undefined;
  const currentLink = useMemo(
    () =>
      getGitHubIssueLink(currentIssue?.extension_metadata ?? null) ??
      initialLink ??
      null,
    [currentIssue, initialLink]
  );

  const [repo, setRepo] = useState(
    initialRepo ?? currentLink?.repo_full_name ?? FALLBACK_GITHUB_REPO
  );
  const [search, setSearch] = useState('');
  const [selectedIssue, setSelectedIssue] = useState<GitHubIssueSummary | null>(
    null
  );
  const [actionError, setActionError] = useState<string | null>(null);
  const [isSaving, setIsSaving] = useState(false);

  useEffect(() => {
    if (!modal.visible) {
      setRepo(
        initialRepo ?? currentLink?.repo_full_name ?? FALLBACK_GITHUB_REPO
      );
      setSearch('');
      setSelectedIssue(null);
      setActionError(null);
      setIsSaving(false);
    }
  }, [
    currentLink?.issue_url,
    currentLink?.repo_full_name,
    initialRepo,
    modal.visible,
  ]);

  const {
    data: searchResults = [],
    isLoading: isSearching,
    error: searchError,
  } = useQuery({
    queryKey: [
      'github-issues',
      hostId ?? 'current',
      repo.trim(),
      search.trim(),
    ],
    queryFn: () => githubIssuesApi.search(repo.trim(), search, { hostId }),
    enabled: modal.visible && repo.trim().length > 0,
    staleTime: 15_000,
  });

  const handleLink = async () => {
    if (!selectedIssue) return;

    setActionError(null);
    setIsSaving(true);

    try {
      const nextLink = createGitHubIssueLinkFromSummary(selectedIssue);
      if (currentIssue && issueId) {
        const nextMetadata = setGitHubIssueLink(
          currentIssue.extension_metadata,
          nextLink
        );
        const { persisted } = updateIssue(issueId, {
          extension_metadata: nextMetadata,
        });
        await persisted;
      }
      modal.resolve({
        action: 'linked',
        link: nextLink,
      });
      modal.hide();
    } catch (error) {
      setActionError(
        error instanceof Error ? error.message : 'Failed to link GitHub issue'
      );
    } finally {
      setIsSaving(false);
    }
  };

  const handleUnlink = async () => {
    if (!currentLink) return;

    setActionError(null);
    setIsSaving(true);

    try {
      if (currentIssue && issueId) {
        const { persisted } = updateIssue(issueId, {
          extension_metadata: clearGitHubIssueLink(
            currentIssue.extension_metadata
          ),
        });
        await persisted;
      }
      modal.resolve({ action: 'unlinked' });
      modal.hide();
    } catch (error) {
      setActionError(
        error instanceof Error ? error.message : 'Failed to unlink GitHub issue'
      );
    } finally {
      setIsSaving(false);
    }
  };

  const issueOptions = selectedIssue
    ? [
        selectedIssue,
        ...searchResults.filter(
          (it) => it.issue_url !== selectedIssue.issue_url
        ),
      ]
    : searchResults;

  return (
    <Dialog
      open={modal.visible}
      onOpenChange={(open) => {
        if (!open) {
          modal.resolve(undefined);
          modal.hide();
        }
      }}
    >
      <DialogContent className="sm:max-w-[620px]">
        <DialogHeader>
          <DialogTitle>Link GitHub Issue</DialogTitle>
          <DialogDescription>
            Tie this task to a GitHub issue for visibility and startup context.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          <div className="rounded-md border bg-muted/30 px-3 py-2 text-sm">
            <div className="font-medium">GitHub issues from {repo}</div>
            <div className="text-muted-foreground">
              This project is linked to that repository, so open issues load
              automatically.
            </div>
          </div>

          <div className="space-y-2">
            <Label htmlFor="github-issue-search">Filter open issues</Label>
            <Input
              id="github-issue-search"
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder="Type to filter, or pick from the loaded list"
              autoFocus
            />
          </div>

          <div className="space-y-2">
            <div className="text-sm font-medium">Available Issues</div>
            <div className="max-h-[240px] space-y-2 overflow-y-auto rounded-md border p-2">
              {isSearching ? (
                <div className="text-sm text-muted-foreground">
                  Loading GitHub issues...
                </div>
              ) : searchError ? (
                <div className="text-sm text-destructive">
                  {searchError instanceof Error
                    ? searchError.message
                    : 'Failed to load GitHub issues'}
                </div>
              ) : issueOptions.length === 0 ? (
                <div className="text-sm text-muted-foreground">
                  {repo.trim()
                    ? 'No matching open issues found.'
                    : 'Project has no linked GitHub repository.'}
                </div>
              ) : (
                issueOptions.map((issue) => {
                  const isSelected =
                    selectedIssue?.issue_url === issue.issue_url;
                  return (
                    <div
                      key={issue.issue_url}
                      className={`flex items-center gap-2 rounded-md border px-2 py-2 ${
                        isSelected
                          ? 'border-primary bg-accent'
                          : 'border-border'
                      }`}
                    >
                      <button
                        type="button"
                        onClick={() => setSelectedIssue(issue)}
                        className="min-w-0 flex-1 text-left"
                      >
                        <div className="truncate font-medium">
                          #{issue.issue_number} {issue.title}
                        </div>
                        <div className="text-xs text-muted-foreground">
                          {issue.repo_full_name} · {issue.state.toLowerCase()}
                        </div>
                      </button>
                      <a
                        href={issue.issue_url}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="flex-shrink-0 text-muted-foreground hover:text-foreground"
                        title="Open in GitHub"
                      >
                        <ArrowSquareOut className="size-4" />
                      </a>
                    </div>
                  );
                })
              )}
            </div>
          </div>

          {currentLink && (
            <div className="rounded-md border border-border bg-muted/40 px-3 py-2 text-sm">
              Currently linked to{' '}
              <a
                href={currentLink.issue_url}
                target="_blank"
                rel="noopener noreferrer"
                className="font-medium underline-offset-4 hover:underline"
              >
                {currentLink.repo_full_name} #{currentLink.issue_number}
              </a>
            </div>
          )}

          {actionError && (
            <div className="text-sm text-destructive">{actionError}</div>
          )}
        </div>

        <DialogFooter>
          {currentLink && (
            <Button
              variant="outline"
              onClick={handleUnlink}
              disabled={isSaving}
            >
              <XIcon className="size-4" />
              Unlink
            </Button>
          )}
          <Button
            variant="outline"
            onClick={() => {
              modal.resolve(undefined);
              modal.hide();
            }}
            disabled={isSaving}
          >
            Cancel
          </Button>
          <Button onClick={handleLink} disabled={!selectedIssue || isSaving}>
            <LinkIcon className="size-4" />
            {isSaving ? 'Linking...' : 'Link Issue'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function LinkGitHubIssueWithContext(props: LinkGitHubIssueDialogProps) {
  if (!props.projectId) {
    return null;
  }

  return (
    <ProjectProvider projectId={props.projectId}>
      <LinkGitHubIssueContent
        issueId={props.issueId}
        hostId={props.hostId}
        initialRepo={props.initialRepo}
        initialLink={props.initialLink}
      />
    </ProjectProvider>
  );
}

const LinkGitHubIssueDialogImpl = create<LinkGitHubIssueDialogProps>(
  (props) => {
    return <LinkGitHubIssueWithContext {...props} />;
  }
);

export const LinkGitHubIssueDialog = defineModal<
  LinkGitHubIssueDialogProps,
  LinkGitHubIssueDialogResult
>(LinkGitHubIssueDialogImpl);
