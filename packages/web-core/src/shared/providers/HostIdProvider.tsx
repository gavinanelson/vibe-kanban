import {
  createContext,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useState,
  type ReactNode,
} from 'react';
import { useCurrentAppDestination } from '@/shared/hooks/useCurrentAppDestination';
import { getDestinationHostId } from '@/shared/lib/routes/appNavigation';
import {
  listPairedRelayHosts,
  subscribeRelayPairingChanges,
} from '@/shared/lib/relayPairingStorage';

type BackendPairedRelayHost = {
  host_id: string;
  host_name?: string;
  paired_at?: string;
};

async function listBackendPairedRelayHosts(): Promise<
  BackendPairedRelayHost[]
> {
  const response = await fetch('/api/relay-auth/client/hosts', {
    headers: { Accept: 'application/json' },
  });

  if (!response.ok) {
    throw new Error(`Failed to list paired relay hosts: ${response.status}`);
  }

  const body = (await response.json()) as {
    success?: boolean;
    data?: { hosts?: BackendPairedRelayHost[] };
  };

  return body.success === true && Array.isArray(body.data?.hosts)
    ? body.data.hosts
    : [];
}

// Module-level getter so the API transport can read the hostId outside React
let _hostId: string | null = null;
export function getCurrentHostId(): string | null {
  return _hostId;
}

const HostIdContext = createContext<string | null>(null);

export function useHostId(): string | null {
  return useContext(HostIdContext);
}

export function HostIdScopeProvider({
  hostId,
  children,
}: {
  hostId: string | null;
  children: ReactNode;
}) {
  return (
    <HostIdContext.Provider value={hostId}>{children}</HostIdContext.Provider>
  );
}

export function HostIdProvider({ children }: { children: ReactNode }) {
  const destination = useCurrentAppDestination();
  const routeHostId = useMemo(
    () => getDestinationHostId(destination),
    [destination]
  );
  const [singlePairedHostId, setSinglePairedHostId] = useState<string | null>(
    null
  );

  useEffect(() => {
    let cancelled = false;

    const refreshSinglePairedHost = async () => {
      try {
        const [indexedDbHosts, backendHosts] = await Promise.allSettled([
          listPairedRelayHosts(),
          listBackendPairedRelayHosts(),
        ]);
        if (cancelled) return;

        const hostIds = new Set<string>();
        if (indexedDbHosts.status === 'fulfilled') {
          for (const host of indexedDbHosts.value) {
            hostIds.add(host.host_id);
          }
        }
        if (backendHosts.status === 'fulfilled') {
          for (const host of backendHosts.value) {
            hostIds.add(host.host_id);
          }
        }

        setSinglePairedHostId(hostIds.size === 1 ? [...hostIds][0] : null);
      } catch (error) {
        if (!cancelled) {
          console.warn('Failed to resolve paired relay hosts', error);
          setSinglePairedHostId(null);
        }
      }
    };

    void refreshSinglePairedHost();
    const unsubscribe = subscribeRelayPairingChanges(() => {
      void refreshSinglePairedHost();
    });

    return () => {
      cancelled = true;
      unsubscribe();
    };
  }, []);

  const hostId = routeHostId ?? singlePairedHostId;

  useLayoutEffect(() => {
    _hostId = hostId;
    return () => {
      _hostId = null;
    };
  }, [hostId]);

  return (
    <HostIdContext.Provider value={hostId}>{children}</HostIdContext.Provider>
  );
}
