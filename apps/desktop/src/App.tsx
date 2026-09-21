import { useDeferredValue, useEffect, useRef, useState, type ReactNode } from 'react';
import { useMutation, useQuery } from '@tanstack/react-query';
import { check } from '@tauri-apps/plugin-updater';
import {
  Activity,
  Bot,
  Boxes,
  CheckCircle2,
  ChevronRight,
  CircleAlert,
  Command,
  FileClock,
  FileDiff,
  FolderKanban,
  Gauge,
  GitBranch,
  KeyRound,
  ListFilter,
  Moon,
  PanelRightOpen,
  Play,
  RefreshCw,
  RotateCcw,
  Search,
  Settings,
  ShieldAlert,
  ShieldCheck,
  Sun,
  TerminalSquare,
  X,
} from 'lucide-react';

import {
  classifyDaemonState,
  executeRecovery,
  exportSession,
  loadAdapters,
  loadDaemonSnapshot,
  loadDashboard,
  loadFiles,
  loadSessionEvents,
  loadSessionFiles,
  planRecovery,
  searchHistory,
  type AdapterView,
  type ConnectionState,
  type DashboardView,
  type EventEnvelope,
  type FindingSummary,
  type SearchResponse,
  type SessionFileChange,
  type SessionView,
} from './lib/daemon';
import { HistoryImport } from './HistoryImport';
import { useHistoryImport } from './lib/history';
import { SessionLauncher } from './SessionLauncher';
import { useThemeStore } from './lib/theme';

type Screen = 'live' | 'sessions' | 'search' | 'files' | 'usage' | 'findings' | 'settings';

const navigation = [
  { id: 'live', label: 'Live', icon: Activity },
  { id: 'sessions', label: 'Sessions', icon: FileClock },
  { id: 'search', label: 'Search', icon: Search },
  { id: 'files', label: 'Files', icon: FolderKanban },
  { id: 'usage', label: 'Usage', icon: Gauge },
  { id: 'findings', label: 'Findings', icon: ShieldCheck },
] satisfies Array<{ id: Screen; label: string; icon: typeof Activity }>;

export function App() {
  const theme = useThemeStore((state) => state.theme);
  const toggleTheme = useThemeStore((state) => state.toggleTheme);
  const [onboarded, setOnboarded] = useState(
    () => window.localStorage.getItem('agenttraceback.onboarding.complete') === 'true',
  );
  const [screen, setScreen] = useState<Screen>('live');
  const [selectedSession, setSelectedSession] = useState<SessionView | null>(null);
  const [selectedEvent, setSelectedEvent] = useState<EventEnvelope | null>(null);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [launcherOpen, setLauncherOpen] = useState(false);
  const pendingG = useRef(false);

  const snapshot = useQuery({
    queryKey: ['daemon-snapshot'],
    queryFn: loadDaemonSnapshot,
    refetchInterval: 5_000,
  });
  const dashboard = useQuery({
    queryKey: ['dashboard'],
    queryFn: loadDashboard,
    refetchInterval: 3_000,
    enabled: onboarded,
  });
  const connection = classifyDaemonState(snapshot.isPending, snapshot.data);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      const typing = target?.tagName === 'INPUT' || target?.tagName === 'TEXTAREA';
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k') {
        event.preventDefault();
        setPaletteOpen(true);
        return;
      }
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'p') {
        event.preventDefault();
        setScreen('search');
        return;
      }
      if (typing) return;
      if (event.key === 'g' || event.key === 'G') {
        pendingG.current = true;
        window.setTimeout(() => {
          pendingG.current = false;
        }, 900);
        return;
      }
      if (pendingG.current) {
        const destination = {
          l: 'live',
          s: 'sessions',
          f: 'files',
        }[event.key.toLowerCase()] as Screen | undefined;
        if (destination) {
          setScreen(destination);
          setSelectedSession(null);
        }
        pendingG.current = false;
      }
      if (event.key === 'Escape') {
        setSelectedEvent(null);
        setPaletteOpen(false);
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, []);

  if (!onboarded) {
    return <Onboarding onComplete={() => setOnboarded(true)} />;
  }

  const openSession = (session: SessionView) => {
    setSelectedSession(session);
    setScreen('sessions');
  };

  return (
    <div className="app-shell">
      <aside className="sidebar" aria-label="Primary navigation">
        <div className="brand">
          <div className="brand-mark" aria-hidden="true">
            <span />
            <span />
            <span />
          </div>
          <div>
            <strong>AgentTraceback</strong>
            <small>verified agent run records</small>
          </div>
        </div>
        <nav className="navigation">
          {navigation.map(({ id, label, icon: Icon }) => (
            <button
              className={screen === id ? 'nav-item nav-item-active' : 'nav-item'}
              key={id}
              type="button"
              aria-current={screen === id ? 'page' : undefined}
              onClick={() => {
                setScreen(id);
                if (id !== 'sessions') setSelectedSession(null);
              }}
            >
              <Icon size={17} strokeWidth={1.8} aria-hidden="true" />
              <span>{label}</span>
            </button>
          ))}
        </nav>
        <div className="sidebar-foot">
          <button
            className={screen === 'settings' ? 'nav-item nav-item-active' : 'nav-item'}
            type="button"
            onClick={() => setScreen('settings')}
          >
            <Settings size={17} strokeWidth={1.8} aria-hidden="true" />
            <span>Settings</span>
          </button>
          <div className="privacy-note">
            <span className="status-dot status-dot-quiet" />
            No outbound telemetry
          </div>
        </div>
      </aside>

      <main className="main-pane">
        <header className="topbar">
          <div className="scope-pill">
            <Boxes size={15} aria-hidden="true" />
            {dashboard.data ? `${dashboard.data.projects.length} projects` : 'All projects'}
          </div>
          <div className="topbar-actions">
            <button className="primary-button" type="button" onClick={() => setLauncherOpen((open) => !open)}><TerminalSquare size={15} /> {launcherOpen ? 'Close recorder' : 'Record session'}</button>
            <button className="command-hint" type="button" onClick={() => setPaletteOpen(true)}>
              <Search size={15} aria-hidden="true" />
              <span>Command palette</span>
              <kbd>{navigator.platform.includes('Mac') ? '⌘' : 'Ctrl'} K</kbd>
            </button>
            <DaemonBadge state={connection} snapshot={snapshot.data} />
            <button
              className="icon-button"
              type="button"
              onClick={toggleTheme}
              aria-label={`Switch to ${theme === 'dark' ? 'light' : 'dark'} theme`}
            >
              {theme === 'dark' ? <Sun size={17} /> : <Moon size={17} />}
            </button>
          </div>
        </header>

        {launcherOpen ? <SessionLauncher /> : null}
        {screen === 'live' ? <HistoryImport /> : null}

        {snapshot.isError || dashboard.isError ? (
          <ErrorPanel
            title="Local daemon unavailable"
            detail={errorText(snapshot.error ?? dashboard.error)}
            retry={() => {
              void snapshot.refetch();
              void dashboard.refetch();
            }}
          />
        ) : null}

        <PageHeader screen={screen} dashboard={dashboard.data} />

        {screen === 'live' ? (
          <LiveScreen dashboard={dashboard.data} onOpenSession={openSession} onOpenEvent={setSelectedEvent} />
        ) : null}
        {screen === 'sessions' ? (
          selectedSession ? (
            <SessionDetail
              session={selectedSession}
              dashboard={dashboard.data}
              onBack={() => setSelectedSession(null)}
              onOpenEvent={setSelectedEvent}
            />
          ) : (
            <SessionsScreen sessions={dashboard.data?.sessions ?? []} onOpenSession={openSession} />
          )
        ) : null}
        {screen === 'search' ? (
          <SearchScreen
            sessions={dashboard.data?.sessions ?? []}
            onOpenSession={openSession}
            onOpenEvent={setSelectedEvent}
          />
        ) : null}
        {screen === 'files' ? (
          <FilesScreen sessions={dashboard.data?.sessions ?? []} onOpenSession={openSession} />
        ) : null}
        {screen === 'usage' ? <UsageScreen dashboard={dashboard.data} /> : null}
        {screen === 'findings' ? (
          <FindingsScreen findings={dashboard.data?.findings ?? []} sessions={dashboard.data?.sessions ?? []} onOpenSession={openSession} />
        ) : null}
        {screen === 'settings' ? <SettingsScreen /> : null}
      </main>

      {selectedEvent ? <EvidenceDrawer event={selectedEvent} onClose={() => setSelectedEvent(null)} /> : null}
      {paletteOpen ? (
        <CommandPalette
          onClose={() => setPaletteOpen(false)}
          onNavigate={(next) => {
            setScreen(next);
            setSelectedSession(null);
            setPaletteOpen(false);
          }}
        />
      ) : null}
    </div>
  );
}

function Onboarding({ onComplete }: { onComplete: () => void }) {
  const adapters = useQuery({ queryKey: ['adapters'], queryFn: loadAdapters });
  const history = useHistoryImport();
  const [step, setStep] = useState(() => {
    const parsed = Number(window.localStorage.getItem('agenttraceback.onboarding.step') ?? '0');
    return Number.isFinite(parsed) ? Math.min(Math.max(parsed, 0), 3) : 0;
  });
  const go = (next: number) => {
    setStep(next);
    window.localStorage.setItem('agenttraceback.onboarding.step', String(next));
  };
  const finish = () => {
    window.localStorage.setItem('agenttraceback.onboarding.complete', 'true');
    window.localStorage.removeItem('agenttraceback.onboarding.step');
    onComplete();
  };

  return (
    <main className="onboarding">
      <div className="onboarding-progress" aria-label={`Step ${step + 1} of 4`}>
        {[0, 1, 2, 3].map((item) => (
          <span className={item <= step ? 'progress-active' : ''} key={item} />
        ))}
      </div>
      <section className="onboarding-card">
        {step === 0 ? (
          <>
            <div className="onboarding-mark"><Command size={30} /></div>
            <span className="panel-kicker">WELCOME</span>
            <h1>One local history for every AI coding agent.</h1>
            <p>No account. No cloud. Nothing leaves this machine by default.</p>
            <div className="privacy-grid">
              <Fact icon={<KeyRound size={18} />} title="Encrypted locally" copy="Raw payloads and recovery content use the operating-system key store." />
              <Fact icon={<ShieldCheck size={18} />} title="Evidence stays honest" copy="Reported, observed, and verified remain distinct." />
              <Fact icon={<RotateCcw size={18} />} title="Recovery is safe" copy="New-directory reconstruction is the default." />
            </div>
            <div className="onboarding-actions">
              <button className="primary-button" type="button" onClick={() => go(1)}>Get started <ChevronRight size={16} /></button>
              <span>Next, find your agents and choose how to get started.</span>
            </div>
          </>
        ) : null}
        {step === 1 ? (
          <>
            <span className="panel-kicker">DETECTION</span>
            <h1>Known agents on this machine</h1>
            <p>AgentTraceback scans known application locations and PATH entries only.</p>
            {adapters.isPending ? <LoadingPanel label="Scanning known locations" /> : null}
            {adapters.isError ? <ErrorPanel title="Adapter scan failed" detail={errorText(adapters.error)} retry={() => void adapters.refetch()} compact /> : null}
            <div className="adapter-grid">
              {(adapters.data?.adapters ?? []).map((adapter) => <AdapterCard adapter={adapter} key={adapter.id} />)}
            </div>
            {!adapters.isPending && (adapters.data?.adapters.length ?? 0) === 0 ? (
              <EmptyState icon={<TerminalSquare />} title="No supported agent installations found" copy="You can still record any CLI agent with the generic wrapper." />
            ) : null}
            <div className="onboarding-actions">
              <button className="primary-button" type="button" onClick={() => go(2)}>Continue <ChevronRight size={16} /></button>
              <button className="ghost-button" type="button" onClick={() => void adapters.refetch()}><RefreshCw size={15} /> Rescan</button>
            </div>
          </>
        ) : null}
        {step === 2 ? (
          <>
            <span className="panel-kicker">CAPTURE OVERVIEW</span>
            <h1>How capture works</h1>
            <p>Next, import existing history or launch a recorded run in your normal terminal.</p>
            <dl className="capture-summary">
              <CaptureDetail title="Historical import" detail="All detected agents import together in the background." />
              <CaptureDetail title="Live sessions" detail="Choose your agent and project, then start recording in your terminal." />
              <CaptureDetail title="Optional agent hooks" detail="Require a separate approval step before installation." />
              <CaptureDetail title="Terminal transcripts" detail="Choose whether to save an encrypted transcript when launching a run." />
              <CaptureDetail title="Sensitive file content" detail="Excluded by default." />
              <CaptureDetail title="Start daemon at login" detail="Optional. Manage with agenttraceback daemon startup in a terminal." />
            </dl>
            <div className="onboarding-actions">
              <button className="primary-button" type="button" onClick={() => go(3)}>Continue <ChevronRight size={16} /></button>
            </div>
          </>
        ) : null}
        {step === 3 ? (
          <>
            <span className="panel-kicker">READY</span>
            <h1>Your local history is ready to grow</h1>
            <p>Open the dashboard to bring your existing agent history together. Sessions appear as they are imported.</p>
            <HistoryImport intro />
            <div className="onboarding-actions">
              <button className="primary-button" type="button" onClick={() => { history.start.mutate(); finish(); }}>Import history and open dashboard <ChevronRight size={16} /></button>
              <button className="ghost-button" type="button" onClick={finish}>Skip import for now</button>
            </div>
          </>
        ) : null}
      </section>
    </main>
  );
}

function PageHeader({ screen, dashboard }: { screen: Screen; dashboard: DashboardView | undefined }) {
  const copy: Record<Screen, [string, string]> = {
    live: ['Live', dashboard?.sessions.some((session) => session.state === 'active') ? 'Active agent runs and host-observed activity.' : 'The latest local agent activity across all projects.'],
    sessions: ['Sessions', 'Historical and live runs with evidence, files, risk, and recovery coverage.'],
    search: ['Search history', 'Search redacted prompts, responses, targets, commands, and file paths.'],
    files: ['Files', 'Cross-session file history and recovery availability.'],
    usage: ['Usage', 'Factual activity totals. No synthetic quality score.'],
    findings: ['Findings', 'Explainable risk matches with their evidence and source.'],
    settings: ['Settings', 'Agents, capabilities, privacy, storage, and diagnostics.'],
  };
  return (
    <header className="page-header">
      <div>
        <span className="panel-kicker">{screen === 'live' ? 'LOCAL RECORDER' : 'AGENTTRACEBACK'}</span>
        <h1>{copy[screen][0]}</h1>
        <p>{copy[screen][1]}</p>
      </div>
    </header>
  );
}

function LiveScreen({
  dashboard,
  onOpenSession,
  onOpenEvent,
}: {
  dashboard: DashboardView | undefined;
  onOpenSession: (session: SessionView) => void;
  onOpenEvent: (event: EventEnvelope) => void;
}) {
  if (!dashboard) return <LoadingPanel label="Loading local history" />;
  const active = dashboard.sessions.filter((session) => ['active', 'finalizing', 'interrupted'].includes(session.state));
  const highRisk = dashboard.findings.filter((finding) => ['high', 'critical'].includes(finding.severity));
  const changedFiles = dashboard.sessions.reduce((total, session) => total + session.fileCount, 0);
  const latestSession = dashboard.sessions[0];
  return (
    <>
      <section className="summary-grid">
        <SummaryCard label="Active agents" value={active.length} detail={`${dashboard.sessions.length} total sessions`} />
        <SummaryCard label="Projects" value={dashboard.projects.length} detail="Local scopes" />
        <SummaryCard label="Recorded events" value={dashboard.totalEvents.toLocaleString()} detail="Append-only local chain" />
        <SummaryCard label="Changed files" value={changedFiles.toLocaleString()} detail="Snapshot coverage varies" />
        <SummaryCard label="High-risk findings" value={highRisk.length} detail={`${dashboard.findings.length} total findings`} tone={highRisk.length > 0 ? 'risk' : undefined} />
        <SummaryCard label="Capture health" value={active.some((session) => session.captureHealth !== 'healthy') ? 'Degraded' : 'Ready'} detail="Unavailable evidence is labeled, not shown empty" />
      </section>
      <section className="dashboard-grid">
        <article className="panel sessions-panel">
          <PanelHeading kicker="ACTIVE + RECENT" title="Agent sessions" action={<button className="text-button" type="button" onClick={() => latestSession && onOpenSession(latestSession)} disabled={!latestSession}>Open latest <ChevronRight size={14} /></button>} />
          {dashboard.sessions.length === 0 ? <EmptyState icon={<Bot />} title="No sessions recorded yet" copy="Run the generic wrapper or import a detected agent's history." /> : (
            <div className="session-list">
              {dashboard.sessions.slice(0, 8).map((session) => <SessionRow key={session.id} session={session} onClick={() => onOpenSession(session)} />)}
            </div>
          )}
        </article>
        <article className="panel">
          <PanelHeading kicker="LOCAL EVIDENCE" title="Recent activity" />
          <Timeline events={dashboard.recentEvents.slice(0, 60)} onOpenEvent={onOpenEvent} compact />
        </article>
      </section>
    </>
  );
}

function SessionsScreen({ sessions, onOpenSession }: { sessions: SessionView[]; onOpenSession: (session: SessionView) => void }) {
  const [agent, setAgent] = useState('all');
  const [query, setQuery] = useState('');
  const agents = [...new Set(sessions.map((session) => session.agentName).filter((value): value is string => Boolean(value)))].sort();
  const filtered = sessions.filter((session) => {
    const matchesAgent = agent === 'all' || session.agentName === agent;
    const haystack = `${session.titlePreview ?? ''} ${session.projectName ?? ''} ${session.modelName ?? ''}`.toLowerCase();
    return matchesAgent && haystack.includes(query.toLowerCase());
  });
  return (
    <section className="panel table-panel">
      <div className="table-toolbar">
        <label className="search-input compact-search">
          <Search size={15} />
          <input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Filter sessions" aria-label="Filter sessions" />
        </label>
        <label className="select-control"><ListFilter size={15} /><select value={agent} onChange={(event) => setAgent(event.target.value)} aria-label="Filter by agent"><option value="all">All agents</option>{agents.map((value) => <option key={value}>{value}</option>)}</select></label>
      </div>
      {filtered.length === 0 ? <EmptyState icon={<FileClock />} title={sessions.length === 0 ? 'No sessions yet' : 'No sessions match this filter'} copy={sessions.length === 0 ? 'Record a new run with `agenttraceback run -- <command>`.' : 'Clear the filter or choose another agent.'} /> : (
        <div className="session-table" role="table" aria-label="Sessions">
          <div className="session-table-head" role="row">
            <span>Started</span><span>Task</span><span>Agent / model</span><span>Project</span><span>Events</span><span>Files</span><span>Risk</span><span />
          </div>
          {filtered.map((session) => (
            <button className="session-table-row" type="button" role="row" key={session.id} onClick={() => onOpenSession(session)}>
              <span>{formatDateTime(session.startedAtUs)}</span>
              <strong>{session.titlePreview ?? 'Untitled agent run'}</strong>
              <span>{session.agentName ?? 'unknown'} · {session.modelName ?? 'model unknown'}</span>
              <span>{session.projectName ?? 'Unattached'}</span>
              <span>{session.eventCount.toLocaleString()}</span>
              <span>{session.fileCount.toLocaleString()}</span>
              <span><SeverityBadge severity={session.riskMaxSeverity} /></span>
              <ChevronRight size={15} />
            </button>
          ))}
        </div>
      )}
    </section>
  );
}

function SessionDetail({
  session,
  dashboard,
  onBack,
  onOpenEvent,
}: {
  session: SessionView;
  dashboard: DashboardView | undefined;
  onBack: () => void;
  onOpenEvent: (event: EventEnvelope) => void;
}) {
  const [tab, setTab] = useState('overview');
  const events = useQuery({ queryKey: ['session-events', session.id], queryFn: () => loadSessionEvents(session.id) });
  const files = useQuery({ queryKey: ['session-files', session.id], queryFn: () => loadSessionFiles(session.id) });
  const exportMutation = useMutation({
    mutationFn: ({ format, full }: { format: 'json' | 'markdown'; full: boolean }) =>
      exportSession(session.id, format, full),
  });
  const findings = (dashboard?.findings ?? []).filter((finding) => finding.sessionId === session.id);
  const tabs = ['overview', 'timeline', 'conversation', 'files', 'processes', 'git', 'usage', 'security', 'recovery'];
  return (
    <section className="detail-layout">
      <aside className="detail-rail">
        <button className="text-button back-button" type="button" onClick={onBack}>← Sessions</button>
        <span className="panel-kicker">SESSION</span>
        {isDemoSession(session) ? <span className="demo-badge">DEMO DATA</span> : null}
        <h2>{session.titlePreview ?? 'Untitled agent run'}</h2>
        <div className="detail-meta">
          <span>{session.agentName ?? 'unknown agent'}</span>
          <span>{session.modelName ?? 'model unknown'}</span>
          <span>{session.projectName ?? 'no project'}</span>
        </div>
        <dl className="detail-stats">
          <Metric label="Events" value={session.eventCount.toLocaleString()} />
          <Metric label="Files" value={session.fileCount.toLocaleString()} />
          <Metric label="Findings" value={session.findingCount.toLocaleString()} />
          <Metric label="Recovery" value={humanize(session.recoveryCoverage)} />
          <Metric label="Outcome" value={humanize(session.outcome)} />
          <Metric label="Capture" value={humanize(session.captureHealth)} />
        </dl>
        <div className="capture-notice"><ShieldAlert size={15} /><span>Historical imports are reported evidence only. Host effects were not captured retroactively.</span></div>
        <button className="ghost-button export-button" type="button" disabled={exportMutation.isPending} onClick={() => exportMutation.mutate({ format: 'markdown', full: false })}>
          <FileDiff size={14} /> {exportMutation.isPending ? 'Exporting…' : 'Export redacted report'}
        </button>
        <button className="ghost-button export-button export-button-warning" type="button" disabled={exportMutation.isPending} onClick={() => exportMutation.mutate({ format: 'json', full: true })}>
          <ShieldAlert size={14} /> Export full local JSON
        </button>
        {exportMutation.data ? <p className="success-copy">{exportMutation.data.path}</p> : null}
        {exportMutation.isError ? <p className="error-copy">{errorText(exportMutation.error)}</p> : null}
      </aside>
      <div className="detail-main">
        <div className="tab-strip" role="tablist">
          {tabs.map((item) => <button className={tab === item ? 'tab-active' : ''} type="button" role="tab" aria-selected={tab === item} key={item} onClick={() => setTab(item)}>{humanize(item)}</button>)}
        </div>
        {events.isPending && tab !== 'recovery' ? <LoadingPanel label="Loading session evidence" /> : null}
        {events.isError && tab !== 'recovery' ? <ErrorPanel title="Timeline unavailable" detail={errorText(events.error)} retry={() => void events.refetch()} compact /> : null}
        {tab === 'overview' ? <Overview session={session} events={events.data ?? []} findings={findings} /> : null}
        {tab === 'timeline' ? <Timeline events={events.data ?? []} onOpenEvent={onOpenEvent} /> : null}
        {tab === 'conversation' ? <Conversation events={events.data ?? []} onOpenEvent={onOpenEvent} /> : null}
        {tab === 'files' ? <FileChanges files={files.data ?? []} loading={files.isPending} /> : null}
        {tab === 'processes' ? (
          <OperationalEvents
            events={events.data ?? []}
            prefixes={['process_']}
            title="Process events"
            emptyTitle="No process events were captured"
            emptyCopy="Process ancestry is best effort. Short-lived descendants may be missing, and historical imports cannot reconstruct host processes."
          />
        ) : null}
        {tab === 'git' ? (
          <OperationalEvents
            events={events.data ?? []}
            prefixes={['git_']}
            title="Git operations"
            emptyTitle="No Git operation events were captured"
            emptyCopy="Wrapper sessions record before/after Git state. This session has no Git events in its normalized timeline."
          />
        ) : null}
        {tab === 'usage' ? <UsageDetail session={session} events={events.data ?? []} /> : null}
        {tab === 'security' ? <SecurityDetail findings={findings} /> : null}
        {tab === 'recovery' ? <RecoveryPanel session={session} files={files.data ?? []} /> : null}
      </div>
    </section>
  );
}

function Overview({ session, events, findings }: { session: SessionView; events: EventEnvelope[]; findings: FindingSummary[] }) {
  const prompts = events.filter((event) => event.action === 'prompt').length;
  const commands = events.filter((event) => event.action === 'command_execute').length;
  const edits = events.filter((event) => event.action.startsWith('file_')).length;
  const tests = events.filter((event) => event.action.startsWith('test_')).length;
  return (
    <div className="overview-grid">
      <article className="panel prose-panel">
        <PanelHeading kicker="DETERMINISTIC SUMMARY" title="What the record shows" />
        <p>{session.titlePreview ?? 'This session has no imported title.'} The record contains {events.length.toLocaleString()} loaded events, {prompts.toLocaleString()} prompts, {commands.toLocaleString()} commands, {edits.toLocaleString()} file actions, and {tests.toLocaleString()} test signals.</p>
        <p>Evidence remains {sessionSourceLabel(session)}. Capture health is {humanize(session.captureHealth)} and recovery coverage is {humanize(session.recoveryCoverage)}.</p>
      </article>
      <article className="panel">
        <PanelHeading kicker="MILESTONES" title="Key activity" />
        <div className="milestone-list">
          {(['prompt', 'file_write', 'command_execute', 'test_result'] as const).map((action) => {
            const match = events.find((event) => event.action === action);
            return <div className="milestone" key={action}><span className={`milestone-dot ${match ? 'milestone-on' : ''}`} /><div><strong>{humanize(action)}</strong><small>{match ? formatDateTime(match.occurredAtUs) : 'Not present in this record'}</small></div></div>;
          })}
        </div>
      </article>
      <article className="panel">
        <PanelHeading kicker="RISK" title="Findings" />
        {findings.length === 0 ? <p className="panel-copy">No explainable findings were attached to this session.</p> : findings.slice(0, 5).map((finding) => <FindingRow finding={finding} key={finding.id} />)}
      </article>
    </div>
  );
}

function Timeline({ events, onOpenEvent, compact = false }: { events: EventEnvelope[]; onOpenEvent: (event: EventEnvelope) => void; compact?: boolean }) {
  const [scrollTop, setScrollTop] = useState(0);
  const rowHeight = compact ? 58 : 68;
  const height = compact ? 470 : 560;
  const overscan = 8;
  const start = Math.max(0, Math.floor(scrollTop / rowHeight) - overscan);
  const count = Math.ceil(height / rowHeight) + overscan * 2;
  const visible = events.slice(start, start + count);
  if (events.length === 0) return <EmptyState icon={<Activity />} title="No timeline events" copy="This session has metadata but no imported normalized events." />;
  return (
    <div className="timeline-viewport" style={{ height }} onScroll={(event) => setScrollTop(event.currentTarget.scrollTop)}>
      <div className="timeline-spacer" style={{ height: events.length * rowHeight }}>
        <div className="timeline-window" style={{ transform: `translateY(${start * rowHeight}px)` }}>
          {visible.map((event) => <TimelineRow compact={compact} event={event} key={event.id} onClick={() => onOpenEvent(event)} />)}
        </div>
      </div>
    </div>
  );
}

function TimelineRow({ event, compact, onClick }: { event: EventEnvelope; compact: boolean; onClick: () => void }) {
  const Icon = actionIcon(event.action);
  return (
    <button className={compact ? 'timeline-row timeline-row-compact' : 'timeline-row'} type="button" onClick={onClick}>
      <span className="timeline-time">{formatTime(event.occurredAtUs)}</span>
      <span className="timeline-icon"><Icon size={15} /></span>
      <span className="timeline-copy"><strong>{humanize(event.action)}</strong><small>{event.target.display ?? event.actor.agent ?? event.source.adapterId ?? 'session'}</small></span>
      <EvidenceBadge evidence={event.evidence.class} />
      <SeverityBadge severity={event.risk.severity} />
    </button>
  );
}

function Conversation({ events, onOpenEvent }: { events: EventEnvelope[]; onOpenEvent: (event: EventEnvelope) => void }) {
  const messages = events.filter((event) => ['prompt', 'response', 'tool_call', 'tool_result'].includes(event.action));
  if (messages.length === 0) return <EmptyState icon={<Bot />} title="No conversation records" copy="The source did not provide prompt, response, or tool semantics." />;
  return <div className="conversation">{messages.map((event) => <button className={`conversation-message conversation-${event.action}`} type="button" key={event.id} onClick={() => onOpenEvent(event)}><div className="conversation-head"><strong>{event.action === 'prompt' ? 'User' : event.action === 'response' ? event.actor.agent ?? 'Agent' : humanize(event.action)}</strong><span>{formatTime(event.occurredAtUs)}</span></div><p>{event.content.redactedPreview ?? event.target.display ?? 'No redacted preview'}</p><EvidenceBadge evidence={event.evidence.class} /></button>)}</div>;
}

function FileChanges({ files, loading }: { files: SessionFileChange[]; loading: boolean }) {
  if (loading) return <LoadingPanel label="Loading file versions" />;
  if (files.length === 0) return <EmptyState icon={<FileDiff />} title="No before/after file versions" copy="Historical adapter imports cannot reconstruct host snapshots retroactively." />;
  return <div className="file-list">{files.map((file) => <article className="file-row" key={file.path}><div className="file-kind">{file.before ? (file.after ? <FileDiff /> : <CircleAlert />) : <CheckCircle2 />}</div><div><strong>{file.path}</strong><small>{file.before ? `${file.before.byteLength ?? '?'} bytes before` : 'Created during session'} → {file.after ? `${file.after.byteLength ?? '?'} bytes after` : 'Deleted after session'}</small></div><span className="hash-chip">{shortHash(file.after?.contentHash ?? file.before?.contentHash)}</span></article>)}</div>;
}

function RecoveryPanel({ session, files }: { session: SessionView; files: SessionFileChange[] }) {
  const [destination, setDestination] = useState(`./agenttraceback-recovery/${session.id.slice(0, 8)}`);
  const mutation = useMutation({ mutationFn: () => planRecovery(session.id, destination) });
  const plan = mutation.data ?? null;
  const execute = useMutation({
    mutationFn: () => {
      if (!plan) throw new Error('Prepare a recovery plan before execution.');
      return executeRecovery(plan.planId, plan.planDigest, true);
    },
  });
  const restorable = files.filter((file) => file.before?.captureStatus === 'hashed').length;
  return (
    <div className="recovery-layout">
      <article className="panel">
        <PanelHeading kicker="SAFE DEFAULT" title="Reconstruct before session" />
        <p className="panel-copy">AgentTraceback builds a new directory and never overwrites the current workspace. {restorable.toLocaleString()} changed files have captured content in this view.</p>
        <label className="field-label">Destination directory<input value={destination} onChange={(event) => setDestination(event.target.value)} /></label>
        <button className="primary-button" type="button" disabled={mutation.isPending} onClick={() => mutation.mutate()}><Play size={15} /> {mutation.isPending ? 'Building plan…' : 'Plan reconstruction'}</button>
        {mutation.isError ? <ErrorPanel title="Recovery planning failed" detail={errorText(mutation.error)} retry={() => mutation.mutate()} compact /> : null}
      </article>
      <article className="panel">
        <PanelHeading kicker="IMMUTABLE PLAN" title={plan ? `${plan.operations.length} operations` : 'No plan prepared'} />
        {plan ? <>
          <div className="plan-summary"><span>Coverage</span><strong>{humanize(plan.coverage)}</strong><span>Digest</span><code>{plan.planDigest.slice(0, 16)}…</code></div>
          <div className="operation-list">{plan.operations.slice(0, 30).map((operation) => <div key={operation.relativePath}><span>{humanize(operation.kind)}</span><strong>{operation.relativePath}</strong></div>)}</div>
          {plan.exclusions.length > 0 ? <p className="panel-copy">{plan.exclusions.length} exclusions are recorded with reasons and will not be silently reconstructed.</p> : null}
          <button className="primary-button" type="button" disabled={execute.isPending} onClick={() => execute.mutate()}><RotateCcw size={15} /> Execute into new directory</button>
          {execute.isError ? <ErrorPanel title="Recovery execution failed" detail={errorText(execute.error)} retry={() => execute.mutate()} compact /> : null}
          {execute.data ? <p className="success-copy">Restored {execute.data.restoredFiles} files; {execute.data.conflictFiles} conflicts.</p> : null}
        </> : <p className="panel-copy">Planning is read-only and produces a digest-bound result for review before execution.</p>}
      </article>
    </div>
  );
}

function SearchScreen({
  sessions,
  onOpenSession,
  onOpenEvent,
}: {
  sessions: SessionView[];
  onOpenSession: (session: SessionView) => void;
  onOpenEvent: (event: EventEnvelope) => void;
}) {
  const [query, setQuery] = useState('');
  const deferred = useDeferredValue(query.trim());
  const result = useQuery({ queryKey: ['search', deferred], queryFn: () => searchHistory(deferred), enabled: deferred.length >= 2 });
  return (
    <section className="search-layout">
      <div className="search-hero">
        <label className="search-input"><Search size={20} /><input autoFocus value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search paths, commands, agents, models, or preview text" aria-label="Search history" /></label>
        <p>Examples: <code>agent:codex</code> <code>path:src/auth</code> <code>evidence:verified</code> <code>risk:high</code></p>
      </div>
      {deferred.length < 2 ? <EmptyState icon={<Search />} title="Search the redacted local index" copy="No decrypted raw payload is needed for indexed search." /> : null}
      {result.isPending && deferred.length >= 2 ? <LoadingPanel label="Searching local index" /> : null}
      {result.isError ? <ErrorPanel title="Search failed" detail={errorText(result.error)} retry={() => void result.refetch()} compact /> : null}
      {result.data && result.data.items.length === 0 ? <EmptyState icon={<Search />} title="No matching events" copy="Try a broader path, agent, or action filter." /> : null}
      {result.data?.items.length ? <div className="search-results">{result.data.items.map((item) => <button className="search-result" type="button" key={item.eventId} onClick={() => { const session = sessions.find((candidate) => candidate.id === item.sessionId); if (session) onOpenSession(session); else onOpenEvent(itemToEvent(item)); }}><div className="search-result-top"><EvidenceBadge evidence={item.evidence as 'reported' | 'observed' | 'verified'} /><span>{humanize(item.action)}</span><time>{formatDateTime(item.occurredAtUs)}</time><SeverityBadge severity={item.risk} /></div><strong>{item.targetDisplay ?? item.projectName ?? 'Unattributed event'}</strong><p>{item.redactedPreview ?? 'No redacted preview'}</p><small>{item.agent ?? 'unknown agent'} · {item.model ?? 'unknown model'} · {item.projectName ?? 'no project'}</small></button>)}</div> : null}
    </section>
  );
}

function FilesScreen({ sessions, onOpenSession }: { sessions: SessionView[]; onOpenSession: (session: SessionView) => void }) {
  const candidates = sessions.filter((session) => session.fileCount > 0);
  const [sessionId, setSessionId] = useState(candidates[0]?.id ?? '');
  const [selectedFileId, setSelectedFileId] = useState<string | null>(null);
  const globalFiles = useQuery({ queryKey: ['global-files'], queryFn: loadFiles });
  const files = useQuery({ queryKey: ['files-screen', sessionId], queryFn: () => loadSessionFiles(sessionId), enabled: Boolean(sessionId) });
  const selected = sessions.find((session) => session.id === sessionId);
  const selectedFile = globalFiles.data?.find((file) => file.id === selectedFileId);
  return (
    <div className="files-layout">
      <aside className="panel project-tree">
        <PanelHeading kicker="CROSS-SESSION HISTORY" title="Known files" />
        {globalFiles.isPending ? <LoadingPanel label="Loading files" /> : null}
        {globalFiles.isError ? <ErrorPanel title="File history unavailable" detail={errorText(globalFiles.error)} retry={() => void globalFiles.refetch()} compact /> : null}
        {globalFiles.data?.length === 0 ? <p className="panel-copy">No file identities have been captured yet.</p> : null}
        {globalFiles.data?.map((file) => <button className={file.id === selectedFileId ? 'tree-item tree-item-active' : 'tree-item'} type="button" key={file.id} onClick={() => setSelectedFileId(file.id)}><FolderKanban size={15} /><span>{file.path}</span><small>{file.versionCount}</small></button>)}
      </aside>
      <section className="panel">
        {selectedFile ? (
          <>
            <PanelHeading kicker="FILE IDENTITY" title={selectedFile.path} />
            <dl className="metric-list">
              <Metric label="Project" value={selectedFile.projectName} />
              <Metric label="Versions" value={selectedFile.versionCount.toLocaleString()} />
              <Metric label="Sessions" value={selectedFile.sessionCount.toLocaleString()} />
              <Metric label="Recoverable" value={selectedFile.recoverableCount.toLocaleString()} />
              <Metric label="Sensitivity" value={selectedFile.sensitiveClass ?? 'not classified'} />
              <Metric label="Last seen" value={formatDateTime(selectedFile.lastSeenAtUs)} />
            </dl>
            <button className="text-button detail-back" type="button" onClick={() => setSelectedFileId(null)}>← Session view</button>
          </>
        ) : (
          <>
            <PanelHeading kicker="SESSION FILE VERSIONS" title={selected?.titlePreview ?? 'Choose a session'} action={selected ? <button className="text-button" type="button" onClick={() => onOpenSession(selected)}>Open session <ChevronRight size={14} /></button> : undefined} />
            <div className="session-file-picker">
              {candidates.map((session) => <button className={session.id === sessionId ? 'filter-chip filter-chip-active' : 'filter-chip'} type="button" key={session.id} onClick={() => setSessionId(session.id)}>{session.titlePreview ?? session.id.slice(0, 8)}</button>)}
            </div>
            {files.isPending && sessionId ? <LoadingPanel label="Loading file versions" /> : null}
            {files.isError ? <ErrorPanel title="Session file history unavailable" detail={errorText(files.error)} retry={() => void files.refetch()} compact /> : null}
            {!sessionId ? <EmptyState icon={<FolderKanban />} title="No before/after file versions" copy="Wrapper or live sessions provide reconstructable file versions. Historical adapter imports cannot reconstruct host snapshots retroactively." /> : <FileChanges files={files.data ?? []} loading={false} />}
          </>
        )}
      </section>
    </div>
  );
}

function UsageScreen({ dashboard }: { dashboard: DashboardView | undefined }) {
  if (!dashboard) return <LoadingPanel label="Loading usage totals" />;
  const commands = dashboard.recentEvents.filter((event) => event.action === 'command_execute').length;
  const tools = dashboard.recentEvents.filter((event) => event.action.includes('tool')).length;
  const edits = dashboard.recentEvents.filter((event) => event.action.startsWith('file_')).length;
  const tests = dashboard.recentEvents.filter((event) => event.action.startsWith('test_')).length;
  const byAgent = countBy(dashboard.sessions, (session) => session.agentName ?? 'unknown');
  return (
    <div className="overview-grid">
      <section className="summary-grid wide-summary">
        <SummaryCard label="Sessions" value={dashboard.sessions.length} detail="Known local history" />
        <SummaryCard label="Events" value={dashboard.totalEvents.toLocaleString()} detail="Stored metadata" />
        <SummaryCard label="Commands" value={commands} detail="Recent loaded window" />
        <SummaryCard label="Tool actions" value={tools} detail="Recent loaded window" />
        <SummaryCard label="File actions" value={edits} detail="Recent loaded window" />
        <SummaryCard label="Test signals" value={tests} detail="Recent loaded window" />
      </section>
      <article className="panel">
        <PanelHeading kicker="AGENT TOTALS" title="Sessions by agent" />
        <div className="bar-list">{[...byAgent.entries()].sort((a, b) => b[1] - a[1]).map(([agent, count]) => <div key={agent}><span>{agent}</span><div><i style={{ width: `${Math.max(8, (count / Math.max(1, dashboard.sessions.length)) * 100)}%` }} /></div><strong>{count}</strong></div>)}</div>
      </article>
      <article className="panel prose-panel">
        <PanelHeading kicker="COST SEMANTICS" title="Token and cost data" />
        <p>Token counts were not supplied by the current records, so AgentTraceback leaves them unknown. When adapters report tokens, the UI labels API-equivalent estimates rather than presenting them as billed cost.</p>
      </article>
    </div>
  );
}

function FindingsScreen({ findings, sessions, onOpenSession }: { findings: FindingSummary[]; sessions: SessionView[]; onOpenSession: (session: SessionView) => void }) {
  return (
    <section className="panel table-panel">
      {findings.length === 0 ? <EmptyState icon={<ShieldCheck />} title="No risk findings" copy="Sensitive paths, dangerous commands, and capture mismatches will appear here with explainable rules." /> : <div className="findings-list">{findings.map((finding) => <article className="finding-card" key={finding.id}><div className="finding-top"><SeverityBadge severity={finding.severity} /><span>{finding.ruleId} v{finding.ruleVersion}</span><time>{formatDateTime(finding.createdAtUs)}</time></div><h3>{finding.title}</h3><p>{finding.explanation}</p>{finding.matchedPreview ? <code>{finding.matchedPreview}</code> : null}<footer><span>{finding.agentName ?? 'unknown agent'} · {finding.projectName ?? 'no project'}</span>{finding.sessionId ? <button className="text-button" type="button" onClick={() => { const session = sessions.find((item) => item.id === finding.sessionId); if (session) onOpenSession(session); }}>Open session <ChevronRight size={14} /></button> : null}</footer></article>)}</div>}
    </section>
  );
}

function SettingsScreen() {
  const [updateMessage, setUpdateMessage] = useState<string | null>(null);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const adapters = useQuery({ queryKey: ['adapters'], queryFn: loadAdapters });
  return (
    <div className="settings-grid">
      <section className="panel settings-section">
        <PanelHeading kicker="AGENTS" title="Detected installations" action={<button className="ghost-button" type="button" onClick={() => void adapters.refetch()}><RefreshCw size={14} /> Rescan</button>} />
        {adapters.isPending ? <LoadingPanel label="Scanning adapters" /> : null}
        {adapters.isError ? <ErrorPanel title="Adapter scan failed" detail={errorText(adapters.error)} retry={() => void adapters.refetch()} compact /> : null}
        <HistoryImport />
        <div className="adapter-grid settings-adapters">{(adapters.data?.adapters ?? []).map((adapter) => <AdapterCard adapter={adapter} key={adapter.id} />)}</div>
      </section>
      <section className="panel settings-section">
        <PanelHeading kicker="PRIVACY" title="Local data boundary" />
        <SettingRow title="Outbound telemetry" value="Disabled" />
        <SettingRow title="Cloud account" value="Not required" />
        <SettingRow title="API binding" value="127.0.0.1 only" />
        <SettingRow title="Default export" value="Redacted" />
        <SettingRow title="Sensitive file content" value="Off" />
      </section>
      <section className="panel settings-section">
        <PanelHeading kicker="STORAGE" title="Retention and integrity" />
        <SettingRow title="Raw payload retention" value="30 days" />
        <SettingRow title="Transcript retention" value="30 days" />
        <SettingRow title="Recovery content" value="Pinned with session" />
        <SettingRow title="Chain verification" value="Run with `agenttraceback verify`" />
      </section>
      <section className="panel settings-section">
        <PanelHeading kicker="ADVANCED" title="Diagnostics" />
        <p className="panel-copy">Support bundles contain redacted logs and metadata only. Nothing is uploaded automatically.</p>
        <code className="command-line">agenttraceback doctor --json</code>
        <code className="command-line">agenttraceback adapters hooks plan claude-code</code>
        <button
          className="ghost-button update-button"
          type="button"
          disabled={checkingUpdate}
          onClick={() => {
            setCheckingUpdate(true);
            void check()
              .then((update) => {
                setUpdateMessage(
                  update
                    ? `AgentTraceback ${update.version} is available. Installation remains user initiated.`
                    : 'AgentTraceback is up to date.',
                );
              })
              .catch(() => {
                setUpdateMessage(
                  'Update checks are not configured in this build or no release endpoint is reachable.',
                );
              })
              .finally(() => setCheckingUpdate(false));
          }}
        >
          <RefreshCw className={checkingUpdate ? 'spin' : ''} size={14} /> Check for updates
        </button>
        {updateMessage ? <p className="panel-copy update-message">{updateMessage}</p> : null}
      </section>
    </div>
  );
}

function CommandPalette({ onClose, onNavigate }: { onClose: () => void; onNavigate: (screen: Screen) => void }) {
  return (
    <div className="modal-backdrop" role="presentation" onMouseDown={onClose}>
      <section className="command-palette" role="dialog" aria-modal="true" aria-label="Command palette" onMouseDown={(event) => event.stopPropagation()}>
        <div className="palette-head"><Command size={17} /><span>Navigate AgentTraceback</span><kbd>Esc</kbd></div>
        <div className="palette-list">
          {navigation.map(({ id, label, icon: Icon }) => <button type="button" key={id} onClick={() => onNavigate(id)}><Icon size={16} /><span>{label}</span><ChevronRight size={14} /></button>)}
          <button type="button" onClick={() => onNavigate('settings')}><Settings size={16} /><span>Settings</span><ChevronRight size={14} /></button>
        </div>
      </section>
    </div>
  );
}

function EvidenceDrawer({ event, onClose }: { event: EventEnvelope; onClose: () => void }) {
  return (
    <aside className="evidence-drawer" aria-label="Evidence drawer">
      <header><div><span className="panel-kicker">EVENT EVIDENCE</span><h2>{humanize(event.action)}</h2></div><button className="icon-button" type="button" onClick={onClose} aria-label="Close evidence drawer"><X size={17} /></button></header>
      <div className="drawer-badges"><EvidenceBadge evidence={event.evidence.class} /><SeverityBadge severity={event.risk.severity} /><span className="hash-chip">{event.source.kind}</span></div>
      <dl className="drawer-facts">
        <Metric label="Occurred" value={formatDateTime(event.occurredAtUs)} />
        <Metric label="Agent" value={event.actor.agent ?? 'unknown'} />
        <Metric label="Model" value={event.actor.model ?? 'unknown'} />
        <Metric label="Attribution" value={humanize(event.evidence.attribution)} />
        <Metric label="Target" value={event.target.display ?? 'none'} />
        <Metric label="Result" value={humanize(event.result.status)} />
      </dl>
      <section><span className="panel-kicker">REDACTED PREVIEW</span><pre>{event.content.redactedPreview ?? 'No preview was provided.'}</pre></section>
      <section><span className="panel-kicker">INTEGRITY</span><pre>{event.integrity ? `sequence ${event.sequence}\n${event.integrity.eventHash}` : 'Not yet finalized'}</pre></section>
      <p className="drawer-note"><PanelRightOpen size={14} /> Raw evidence remains local and encrypted. This drawer never renders recorded content as HTML.</p>
    </aside>
  );
}

function AdapterCard({ adapter, onImport, importing = false }: { adapter: AdapterView; onImport?: (() => void) | undefined; importing?: boolean | undefined }) {
  return (
    <article className="adapter-card">
      <div className="adapter-card-head"><div className="adapter-icon"><Bot size={17} /></div><div><strong>{adapter.displayName}</strong><small>{adapter.agentVersion ?? 'version unknown'}</small></div><span className={`status-pill status-${adapter.status}`}>{adapter.status}</span></div>
      {adapter.diagnosticCode ? <p className="adapter-warning">{humanize(adapter.diagnosticCode)}</p> : null}
      <div className="adapter-capabilities">{adapter.capabilities.filter((capability) => ['historical_sessions', 'live_sessions', 'tool_calls', 'generic_wrapper'].includes(capability.name)).map((capability) => <span key={capability.name}>{humanize(capability.name)}: <b>{capability.availability}</b></span>)}</div>
      {onImport ? <button className="ghost-button" type="button" disabled={importing} onClick={onImport}>{importing ? <RefreshCw className="spin" size={14} /> : <Play size={14} />} Import history</button> : null}
    </article>
  );
}

function SessionRow({ session, onClick }: { session: SessionView; onClick: () => void }) {
  return <button className="session-row" type="button" onClick={onClick}><div className={`agent-orb agent-${agentClass(session.agentName)}`}><Bot size={16} /></div><div><strong>{session.titlePreview ?? 'Untitled agent run'}</strong><small>{session.agentName ?? 'unknown'} · {session.projectName ?? 'unattached'} · {session.eventCount.toLocaleString()} events</small></div><div className="session-row-right"><time>{formatRelative(session.startedAtUs)}</time><SeverityBadge severity={session.riskMaxSeverity} /></div></button>;
}

function FindingRow({ finding }: { finding: FindingSummary }) {
  return <div className="finding-row"><SeverityBadge severity={finding.severity} /><div><strong>{finding.title}</strong><small>{finding.explanation}</small></div></div>;
}

function SecurityDetail({ findings }: { findings: FindingSummary[] }) {
  if (findings.length === 0) return <CapabilityNotice title="No findings in this session" copy="This does not mean every file read or network connection was observed. Standard capture does not provide that guarantee." />;
  return <div className="findings-list">{findings.map((finding) => <article className="finding-card" key={finding.id}><div className="finding-top"><SeverityBadge severity={finding.severity} /><span>{finding.ruleId}</span></div><h3>{finding.title}</h3><p>{finding.explanation}</p>{finding.matchedPreview ? <code>{finding.matchedPreview}</code> : null}</article>)}</div>;
}

function UsageDetail({ session, events }: { session: SessionView; events: EventEnvelope[] }) {
  const commands = events.filter((event) => event.action === 'command_execute').length;
  const tools = events.filter((event) => event.action.includes('tool')).length;
  const failures = events.filter((event) => event.result.status === 'failed').length;
  return <div className="overview-grid"><article className="panel"><PanelHeading kicker="CAPTURE FACTS" title="Activity counts" /><dl className="metric-list"><Metric label="Commands" value={String(commands)} /><Metric label="Tools" value={String(tools)} /><Metric label="Failures" value={String(failures)} /><Metric label="Model" value={session.modelName ?? 'unknown'} /><Metric label="Provider" value={session.providerName ?? 'unknown'} /><Metric label="Harness" value={session.harnessName ?? 'unknown'} /></dl></article><CapabilityNotice title="Token accounting unavailable" copy="No adapter reported input, output, cache, or reasoning token counts for this record. Cost is therefore unknown rather than estimated as zero." /></div>;
}

function OperationalEvents({
  events,
  prefixes,
  title,
  emptyTitle,
  emptyCopy,
}: {
  events: EventEnvelope[];
  prefixes: string[];
  title: string;
  emptyTitle: string;
  emptyCopy: string;
}) {
  const filtered = events.filter((event) => prefixes.some((prefix) => event.action.startsWith(prefix)));
  if (filtered.length === 0) {
    return <CapabilityNotice title={emptyTitle} copy={emptyCopy} />;
  }
  return (
    <article className="panel">
      <PanelHeading kicker="HOST + AGENT PROJECTION" title={title} />
      <div className="file-list">
        {filtered.map((event) => (
          <div className="file-row" key={event.id}>
            <div className="file-kind">{actionIcon(event.action)({ size: 15 })}</div>
            <div>
              <strong>{humanize(event.action)}</strong>
              <small>
                {event.target.display ?? event.actor.agent ?? 'unknown target'} · {formatDateTime(event.occurredAtUs)} · {humanize(event.evidence.attribution)}
              </small>
            </div>
            <EvidenceBadge evidence={event.evidence.class} />
          </div>
        ))}
      </div>
    </article>
  );
}

function CapabilityNotice({ title, copy }: { title: string; copy: string }) {
  return <article className="capability-notice"><CircleAlert size={19} /><div><strong>{title}</strong><p>{copy}</p></div></article>;
}

function SummaryCard({ label, value, detail, tone }: { label: string; value: string | number; detail: string; tone?: 'risk' | undefined }) {
  return <article className={tone ? `summary-card summary-card-${tone}` : 'summary-card'}><span>{label}</span><strong>{value}</strong><small>{detail}</small></article>;
}

function PanelHeading({ kicker, title, action }: { kicker: string; title: string; action?: ReactNode }) {
  return <div className="panel-heading"><div><span className="panel-kicker">{kicker}</span><h2>{title}</h2></div>{action}</div>;
}

function Metric({ label, value }: { label: string; value: string }) {
  return <div><dt>{label}</dt><dd>{value}</dd></div>;
}

function EvidenceBadge({ evidence }: { evidence: 'reported' | 'observed' | 'verified' }) {
  return <span className={`evidence-badge evidence-${evidence}`}>{evidence.toUpperCase()}</span>;
}

function SeverityBadge({ severity }: { severity: string }) {
  const normalized = severity === 'none' ? 'none' : severity;
  return <span className={`severity-badge severity-${normalized}`}>{humanize(severity)}</span>;
}

function DaemonBadge({ state, snapshot }: { state: ConnectionState; snapshot: { health: { port: number } } | undefined }) {
  const label = { connecting: 'Connecting', connected: 'Daemon healthy', unavailable: 'Daemon offline' }[state];
  return <div className={`daemon-badge daemon-badge-${state}`}><span className="status-dot" /><span>{label}</span>{snapshot ? <code>: {snapshot.health.port}</code> : null}</div>;
}

function EmptyState({ icon, title, copy }: { icon: ReactNode; title: string; copy: string }) {
  return <div className="empty-state"><div className="empty-icon">{icon}</div><div><strong>{title}</strong><p>{copy}</p></div></div>;
}

function LoadingPanel({ label }: { label: string }) {
  return <div className="loading-panel" role="status"><span className="activity-pulse" /><span>{label}…</span></div>;
}

function ErrorPanel({ title, detail, retry, compact = false }: { title: string; detail: string; retry: () => void; compact?: boolean }) {
  return <div className={compact ? 'error-panel error-panel-compact' : 'error-panel'} role="alert"><div><strong>{title}</strong><p>{detail}</p></div><button type="button" onClick={retry}><RefreshCw size={14} /> Retry</button></div>;
}

function Fact({ icon, title, copy }: { icon: ReactNode; title: string; copy: string }) {
  return <div className="fact"><span>{icon}</span><strong>{title}</strong><p>{copy}</p></div>;
}

function CaptureDetail({ title, detail }: { title: string; detail: string }) {
  return <div className="capture-detail"><dt>{title}</dt><dd>{detail}</dd></div>;
}

function SettingRow({ title, value }: { title: string; value: string }) {
  return <div className="setting-row"><span>{title}</span><strong>{value}</strong></div>;
}

function actionIcon(action: string) {
  if (action.includes('command')) return TerminalSquare;
  if (action.startsWith('file_')) return FileDiff;
  if (action.startsWith('git_')) return GitBranch;
  if (action.includes('risk')) return ShieldAlert;
  return Activity;
}

function agentClass(agent: string | null): string {
  if (!agent) return 'unknown';
  if (agent.includes('codex')) return 'codex';
  if (agent.includes('claude')) return 'claude';
  if (agent.includes('hermes')) return 'hermes';
  if (agent.includes('opencode')) return 'opencode';
  if (agent.includes('gemini')) return 'gemini';
  return 'unknown';
}

function humanize(value: string): string {
  return value.replaceAll('_', ' ').replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function errorText(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === 'string') return error;
  return 'The local operation could not be completed.';
}

function formatDateTime(timestampUs: number): string {
  return new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'medium' }).format(new Date(timestampUs / 1_000));
}

function formatTime(timestampUs: number): string {
  return new Intl.DateTimeFormat(undefined, { hour: '2-digit', minute: '2-digit', second: '2-digit' }).format(new Date(timestampUs / 1_000));
}

function formatRelative(timestampUs: number): string {
  const deltaSeconds = Math.round((timestampUs - Date.now() * 1_000) / 1_000_000);
  const formatter = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });
  if (Math.abs(deltaSeconds) < 60) return formatter.format(deltaSeconds, 'second');
  const minutes = Math.round(deltaSeconds / 60);
  if (Math.abs(minutes) < 60) return formatter.format(minutes, 'minute');
  const hours = Math.round(minutes / 60);
  if (Math.abs(hours) < 24) return formatter.format(hours, 'hour');
  return formatter.format(Math.round(hours / 24), 'day');
}

function shortHash(hash: string | null | undefined): string {
  return hash ? `${hash.slice(0, 12)}…` : 'metadata only';
}

function sessionSourceLabel(session: SessionView): string {
  return session.captureHealth === 'historical_import'
    ? 'reported-only historical evidence'
    : 'mixed reported and observed evidence';
}

function isDemoSession(session: SessionView): boolean {
  return session.projectName?.includes('Demo') ?? false;
}

function countBy<T>(items: T[], key: (item: T) => string): Map<string, number> {
  const counts = new Map<string, number>();
  for (const item of items) counts.set(key(item), (counts.get(key(item)) ?? 0) + 1);
  return counts;
}

function itemToEvent(item: SearchResponse['items'][number]): EventEnvelope {
  return {
    schemaVersion: 1,
    id: item.eventId,
    sessionId: item.sessionId,
    projectId: null,
    occurredAtUs: item.occurredAtUs,
    observedAtUs: item.occurredAtUs,
    monotonicNs: null,
    sequence: null,
    source: { kind: 'search_projection', originalSourceKind: null, adapterId: item.agent, sourceEventId: item.eventId, rawBlobId: null },
    action: item.action,
    rawAction: null,
    actor: { agent: item.agent, model: item.model, pid: null, parentPid: null, subagentId: null },
    target: { kind: 'unknown', display: item.targetDisplay, normalizedPath: null, external: false },
    result: { status: item.status, exitCode: null, durationMs: null },
    content: { redactedPreview: item.redactedPreview, payloadBlobId: null, beforeHash: null, afterHash: null, bytesChanged: null },
    evidence: { class: item.evidence as 'reported' | 'observed' | 'verified', attribution: 'unknown', correlationVersion: null },
    risk: { severity: item.risk, findingIds: [] },
    integrity: null,
  };
}
