import { useQuery } from '@tanstack/react-query';
import { workspacesApi } from '@/shared/lib/api';
import { useHostId } from '@/shared/providers/HostIdProvider';
import { getHostRequestScopeQueryKey } from '@/shared/lib/hostRequestScope';

export function useWorkspaceBranch(workspaceId?: string) {
  const hostId = useHostId();

  const query = useQuery({
    queryKey: [
      'attemptBranch',
      getHostRequestScopeQueryKey(hostId),
      workspaceId,
    ],
    queryFn: async () => {
      const attempt = await workspacesApi.get(workspaceId!, hostId);
      return attempt.branch ?? null;
    },
    enabled: !!workspaceId,
  });

  return {
    branch: query.data ?? null,
    isLoading: query.isLoading,
    refetch: query.refetch,
  } as const;
}
