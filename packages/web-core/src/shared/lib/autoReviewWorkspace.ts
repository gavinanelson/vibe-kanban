export type AutoReviewLinkedWorkspace = {
  archived?: boolean | null;
  local_workspace_id?: string | null;
};

export const getAutoReviewLocalWorkspaceId = (
  workspaces: AutoReviewLinkedWorkspace[],
  localWorkspacesById: ReadonlyMap<string, unknown>
): string | null => {
  const workspace = workspaces.find(
    (workspace) =>
      !workspace.archived &&
      !!workspace.local_workspace_id &&
      localWorkspacesById.has(workspace.local_workspace_id)
  );

  return workspace?.local_workspace_id ?? null;
};
