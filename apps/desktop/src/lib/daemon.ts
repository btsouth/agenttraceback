import { invoke } from '@tauri-apps/api/core';

export interface SubsystemHealth {
  status: string;
  detail: string | null;
}

export interface HealthResponse {
  status: string;
  apiVersion: number;
  daemonVersion: string;
  pid: number;
  port: number;
  startedAtUs: number;
  database: SubsystemHealth;
  capture: SubsystemHealth;
  keyProtection: string;
}

export interface StandardCaptureCapabilities {
  exactWrapperCommand: boolean;
  hostObservedWrites: boolean;
  hostObservedReads: boolean;
  hostObservedNetwork: boolean;
  bestEffortProcessAncestry: boolean;
  gitBoundaries: boolean;
}

export interface CapabilitiesResponse {
  platform: string;
  deepCapture: boolean;
  standardCapture: StandardCaptureCapabilities;
}

export interface DesktopSnapshot {
  health: HealthResponse;
  capabilities: CapabilitiesResponse;
}

export interface ProjectView {
  id: string;
  displayName: string;
  canonicalRoot: string;
  vcsKind: string;
  lastSeenAtUs: number;
  sessionCount: number;
  eventCount: number;
}

export interface SessionView {
  id: string;
  projectId: string | null;
  projectName: string | null;
  titlePreview: string | null;
  state: string;
  startedAtUs: number;
  endedAtUs: number | null;
  agentName: string | null;
  harnessName: string | null;
  providerName: string | null;
  modelName: string | null;
  outcome: string;
  outcomeConfidence: string;
  recoveryCoverage: string;
  captureHealth: string;
  riskMaxSeverity: string;
  eventCount: number;
  fileCount: number;
  findingCount: number;
}

export interface EventEnvelope {
  schemaVersion: number;
  id: string;
  sessionId: string | null;
  projectId: string | null;
  occurredAtUs: number;
  observedAtUs: number;
  monotonicNs: number | null;
  sequence: number | null;
  source: {
    kind: string;
    originalSourceKind: string | null;
    adapterId: string | null;
    sourceEventId: string;
    rawBlobId: string | null;
  };
  action: string;
  rawAction: string | null;
  actor: {
    agent: string | null;
    model: string | null;
    pid: number | null;
    parentPid: number | null;
    subagentId: string | null;
  };
  target: {
    kind: string;
    display: string | null;
    normalizedPath: string | null;
    external: boolean;
  };
  result: {
    status: string;
    exitCode: number | null;
    durationMs: number | null;
  };
  content: {
    redactedPreview: string | null;
    payloadBlobId: string | null;
    beforeHash: string | null;
    afterHash: string | null;
    bytesChanged: number | null;
  };
  evidence: {
    class: 'reported' | 'observed' | 'verified';
    attribution: string;
    correlationVersion: number | null;
  };
  risk: {
    severity: string;
    findingIds: string[];
  };
  integrity: {
    previousHash: string;
    eventHash: string;
  } | null;
}

export interface FindingSummary {
  id: string;
  sessionId: string | null;
  eventId: string;
  projectName: string | null;
  agentName: string | null;
  ruleId: string;
  ruleVersion: string;
  severity: string;
  status: string;
  title: string;
  explanation: string;
  matchedPreview: string | null;
  createdAtUs: number;
}

export interface DashboardView {
  generatedAtUs: number;
  totalEvents: number;
  projects: ProjectView[];
  sessions: SessionView[];
  recentEvents: EventEnvelope[];
  findings: FindingSummary[];
}

export interface SearchItem {
  eventId: string;
  sessionId: string | null;
  occurredAtUs: number;
  action: string;
  agent: string | null;
  model: string | null;
  projectName: string | null;
  targetDisplay: string | null;
  redactedPreview: string | null;
  evidence: string;
  status: string;
  risk: string;
  rank: number | null;
}

export interface SearchResponse {
  items: SearchItem[];
  hasMore: boolean;
}

export interface AdapterCapability {
  name: string;
  availability: string;
  detail: string | null;
}

export interface AdapterView {
  id: string;
  displayName: string;
  installationId: string;
  agentVersion: string | null;
  sourceRoots: string[];
  status: string;
  diagnosticCode: string | null;
  capabilities: AdapterCapability[];
}

export interface AdapterScanResponse {
  adapters: AdapterView[];
}

export interface AdapterImportResponse {
  adapterId: string;
  sources: number;
  eventsImported: number;
  quarantined: number;
  warnings: string[];
}

export interface FileVersionView {
  versionId: string;
  contentHash: string | null;
  byteLength: number | null;
  captureStatus: string;
  executable: boolean;
  symlinkTarget: string | null;
}

export interface SessionFileChange {
  path: string;
  before: FileVersionView | null;
  after: FileVersionView | null;
}

export interface FileSummary {
  id: string;
  projectName: string;
  path: string;
  sensitiveClass: string | null;
  firstSeenAtUs: number;
  lastSeenAtUs: number;
  versionCount: number;
  sessionCount: number;
  recoverableCount: number;
}

export interface RecoveryOperationView {
  relativePath: string;
  kind: string;
  expectedHash: string | null;
  expectedCurrentHash: string | null;
  byteLength: number | null;
  executable: boolean;
}

export interface RecoveryExclusionView {
  relativePath: string;
  reason: string;
}

export interface RecoveryPlanView {
  planId: string;
  sessionId: string;
  action: string;
  destination: string;
  coverage: string;
  planDigest: string;
  operations: RecoveryOperationView[];
  exclusions: RecoveryExclusionView[];
}

export interface RecoveryRunView {
  backupPlanId: string | null;
  runId: string;
  planId: string;
  state: string;
  restoredFiles: number;
  skippedFiles: number;
  conflictFiles: number;
  errorCode: string | null;
}

export interface ExportView {
  id: string;
  state: string;
  path: string;
  format: string;
  redacted: boolean;
}

export type ConnectionState = 'connecting' | 'connected' | 'unavailable';

export const loadDaemonSnapshot = () => invoke<DesktopSnapshot>('daemon_snapshot');
export const loadDashboard = () => invoke<DashboardView>('dashboard');
export const loadSessions = () => invoke<SessionView[]>('sessions');
export const loadSession = (sessionId: string) => invoke<SessionView>('session', { sessionId });
export const loadSessionEvents = (sessionId: string) =>
  invoke<EventEnvelope[]>('session_events', { sessionId });
export const loadSessionFiles = (sessionId: string) =>
  invoke<SessionFileChange[]>('session_files', { sessionId });
export const loadFiles = () => invoke<FileSummary[]>('files');
export const loadFindings = () => invoke<FindingSummary[]>('findings');
export const exportSession = (
  sessionId: string,
  format: 'json' | 'markdown',
  full = false,
) => invoke<ExportView>('export_session', { sessionId, format, full });
export const searchHistory = (query: string, limit = 100) =>
  invoke<SearchResponse>('search_history', { query, limit });
export const loadAdapters = () => invoke<AdapterScanResponse>('adapters');
export const importAdapter = (adapterId: string) =>
  invoke<AdapterImportResponse>('import_adapter', { adapterId });
export const planRecovery = (sessionId: string, destination: string, paths: string[] = []) =>
  invoke<RecoveryPlanView>('plan_recovery', {
    request: { sessionId, action: 'reconstruct_pre_session', destination, paths },
  });
export const executeRecovery = (planId: string, planDigest: string, confirmed: boolean) =>
  invoke<RecoveryRunView>('execute_recovery', {
    planId,
    request: { planDigest, confirm: confirmed, overwriteConflicts: false },
  });

export function classifyDaemonState(
  isPending: boolean,
  data: DesktopSnapshot | undefined,
): ConnectionState {
  if (isPending) return 'connecting';
  return data?.health.status === 'ok' ? 'connected' : 'unavailable';
}

export interface HistoryImportStatus {
  id: string | null;
  status: 'idle' | 'running' | 'completed' | 'completed_with_errors';
  error: string | null;
  agents: Array<{ adapterId: string; displayName: string; status: 'queued' | 'running' | 'completed' | 'failed'; sources: number; totalSources: number; eventsImported: number; quarantined: number; error: string | null }>;
}
export const loadHistoryImport = () => invoke<HistoryImportStatus>('history_import_status');
export const startHistoryImport = () => invoke<HistoryImportStatus>('start_history_import');
