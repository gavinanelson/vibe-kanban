import {
  createContext,
  useContext,
  useLayoutEffect,
  useMemo,
  type ReactNode,
} from 'react';
import { useCurrentAppDestination } from '@/shared/hooks/useCurrentAppDestination';
import { getDestinationHostId } from '@/shared/lib/routes/appNavigation';

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
  // Do not globally fall back to the only paired host. Host-aware calls on local
  // routes must stay local unless the current route or a scoped provider
  // explicitly supplies a host id.
  const hostId = routeHostId;

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
