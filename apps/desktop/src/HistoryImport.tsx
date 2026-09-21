import { RefreshCw } from 'lucide-react';
import { useHistoryImport } from './lib/history';

export function HistoryImport({ intro = false }: { intro?: boolean }) {
  const { progress, start, starting, startError } = useHistoryImport();
  const status = progress.data;
  const busy = status?.status === 'running' || starting;
  const total = status?.agents.reduce((sum, agent) => sum + agent.eventsImported, 0) ?? 0;
  const failed = status?.status === 'completed_with_errors';
  if (intro) return <section className="history-import"><h2>Your agents, imported together</h2><p>Open the dashboard to import all detected history in the background. Browse sessions as they arrive. You can close this window without stopping the import.</p></section>;
  return <section className="history-import" aria-label="History import">
    <div className="history-import-header"><div><h2>{busy ? 'Importing your history in the background' : failed ? 'History imported with some problems' : status?.status === 'completed' ? 'Last history import complete' : 'Bring your existing history together'}</h2>
      <p>{busy ? `${total.toLocaleString()} events imported. You can keep using the app.` : status?.status === 'completed' ? `${total.toLocaleString()} events imported from ${status.agents.length} agents in the last import.` : 'One action imports all detected agents. Progress is saved as each batch finishes.'}</p></div>
      <button className="primary-button" type="button" disabled={busy || progress.isPending} onClick={() => start.mutate()}><RefreshCw size={15} className={busy ? 'spin' : ''} />{busy ? 'Importing…' : failed ? 'Retry unfinished imports' : status?.status === 'completed' ? 'Check for new history' : 'Import all detected history'}</button>
    </div>
    {status?.agents.length ? <details open={busy || failed}><summary>Agent progress</summary><ul className="import-progress-list">{status.agents.map((agent) => <li key={agent.adapterId}><strong>{agent.displayName}</strong><span>{agent.status === 'queued' ? 'Waiting' : agent.status === 'running' ? `Importing · ${agent.sources}/${agent.totalSources} sources · ${agent.eventsImported.toLocaleString()} events` : agent.status === 'failed' ? 'Needs attention' : `${agent.eventsImported.toLocaleString()} events imported`}{agent.quarantined ? ` · ${agent.quarantined} records skipped` : ''}</span>{agent.error ? <p className="launch-error">{agent.error}</p> : null}</li>)}</ul></details> : busy ? <p role="status">Finding installed agents and their history…</p> : null}
    {progress.isError ? <p role="alert">Could not check import progress. Retrying automatically; an ongoing import keeps running.</p> : null}
    {startError ? <p role="alert" className="launch-error">Could not start the import. {String(startError)}</p> : null}
    {status?.error ? <p role="alert" className="launch-error">{status.error}</p> : null}
  </section>;
}
