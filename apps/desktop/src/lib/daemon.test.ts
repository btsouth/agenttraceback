import { describe, expect, it } from 'vitest';

import { classifyDaemonState, type DesktopSnapshot } from './daemon';

const connectedSnapshot: DesktopSnapshot = {
  health: {
    status: 'ok',
    apiVersion: 1,
    daemonVersion: '0.1.0-dev',
    pid: 123,
    port: 4567,
    startedAtUs: 0,
    database: { status: 'not_initialized', detail: null },
    capture: { status: 'idle', detail: null },
    keyProtection: 'Device permissions only',
  },
  capabilities: {
    platform: 'linux',
    deepCapture: false,
    standardCapture: {
      exactWrapperCommand: false,
      hostObservedWrites: false,
      hostObservedReads: false,
      hostObservedNetwork: false,
      bestEffortProcessAncestry: false,
      gitBoundaries: false,
    },
  },
};

describe('classifyDaemonState', () => {
  it('reports initial loading state', () => {
    expect(classifyDaemonState(true, undefined)).toBe('connecting');
  });

  it('reports a healthy daemon as connected', () => {
    expect(classifyDaemonState(false, connectedSnapshot)).toBe('connected');
  });

  it('reports missing health as unavailable', () => {
    expect(classifyDaemonState(false, undefined)).toBe('unavailable');
  });
});
