export type AutoReviewLinkedWorkspace = {
  archived?: boolean | null;
  local_workspace_id?: string | null;
};

export type AutoReviewWorkspaceResolution =
  | {
      state: 'ready';
      localWorkspaceId: string;
    }
  | {
      state: 'pending-local-workspace';
    }
  | {
      state: 'no-linked-workspace';
    };

export const getAutoReviewWorkspaceResolution = (
  workspaces: AutoReviewLinkedWorkspace[],
  localWorkspacesById: ReadonlyMap<string, unknown>
): AutoReviewWorkspaceResolution => {
  const linkedWorkspace = workspaces.find(
    (workspace) =>
      !workspace.archived &&
      !!workspace.local_workspace_id &&
      localWorkspacesById.has(workspace.local_workspace_id)
  );

  if (linkedWorkspace?.local_workspace_id) {
    return {
      state: 'ready',
      localWorkspaceId: linkedWorkspace.local_workspace_id,
    };
  }

  const hasPendingLocalWorkspace = workspaces.some(
    (workspace) => !workspace.archived && !!workspace.local_workspace_id
  );

  if (hasPendingLocalWorkspace) {
    return { state: 'pending-local-workspace' };
  }

  return { state: 'no-linked-workspace' };
};

export const getAutoReviewLocalWorkspaceId = (
  workspaces: AutoReviewLinkedWorkspace[],
  localWorkspacesById: ReadonlyMap<string, unknown>
): string | null => {
  const resolution = getAutoReviewWorkspaceResolution(
    workspaces,
    localWorkspacesById
  );

  return resolution.state === 'ready' ? resolution.localWorkspaceId : null;
};
