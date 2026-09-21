import { useEffect } from 'react';
import { useIsMutating, useMutation, useMutationState, useQuery, useQueryClient } from '@tanstack/react-query';
import { loadHistoryImport, startHistoryImport } from './daemon';

export function useHistoryImport() {
  const client = useQueryClient();
  const progress = useQuery({ queryKey: ['history-import'], queryFn: loadHistoryImport, refetchInterval: 1500 });
  const start = useMutation({ mutationKey: ['history-import-start'], mutationFn: startHistoryImport, onSuccess: (status) => {
    client.setQueryData(['history-import'], status);
  } });
  useEffect(() => {
    if (progress.data && progress.data.status !== 'idle') void client.invalidateQueries({ queryKey: ['dashboard'] });
  }, [client, progress.data]);
  const starting = useIsMutating({ mutationKey: ['history-import-start'] }) > 0;
  const attempts = useMutationState({ filters: { mutationKey: ['history-import-start'] }, select: (mutation) => ({ status: mutation.state.status, error: mutation.state.error }) });
  const latest = attempts.at(-1);
  return { progress, start, starting, startError: latest?.status === 'error' ? latest.error : null };
}

