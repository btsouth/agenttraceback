import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { expect, test, type Page } from '@playwright/test';

const harness = JSON.parse(
  fs.readFileSync(path.join(os.tmpdir(), 'agenttraceback-playwright-harness.json'), 'utf8'),
) as { baseUrl: string; token: string };

async function installBridge(page: Page, importFixture = false) {
  await page.addInitScript(
    ({ baseUrl, token, importFixture }) => {
      const api = async (route: string, init?: RequestInit) => {
        const response = await fetch(`${baseUrl}${route}`, {
          ...init,
          headers: {
            authorization: `Bearer ${token}`,
            'content-type': 'application/json',
            ...(init?.headers ?? {}),
          },
        });
        const body = await response.text();
        if (!response.ok) throw new Error(`${response.status}: ${body}`);
        return body ? JSON.parse(body) : null;
      };
      const internals = {
        invoke: async (command: string, args: Record<string, unknown> = {}) => {
          switch (command) {
            case 'pick_project_folder':
              return '/tmp/project with spaces';
            case 'launch_recorded_session':
              localStorage.setItem('test.launch', JSON.stringify(args));
              if (args.project === '/missing') throw new Error('Cannot open project folder');
              return { terminal: 'test terminal' };
            case 'copy_text':
              localStorage.setItem('test.clipboard', String(args.text));
              return;
            case 'paste_text':
              return localStorage.getItem('test.clipboard') ?? '';
            case 'daemon_snapshot':
              return {
                health: await api('/api/v1/health'),
                capabilities: await api('/api/v1/capabilities'),
              };
            case 'dashboard':
              return api('/api/v1/dashboard');
            case 'sessions':
              return api('/api/v1/sessions?limit=200');
            case 'session':
              return api(`/api/v1/sessions/${args.sessionId}`);
            case 'session_events':
              return api(`/api/v1/sessions/${args.sessionId}/events?limit=500`);
            case 'session_files':
              return api(`/api/v1/sessions/${args.sessionId}/files`);
            case 'findings':
              return api('/api/v1/findings?limit=200');
            case 'adapters':
              if (importFixture) return { adapters: [{ id: 'codex', displayName: 'Codex', status: 'available', capabilities: [{ name: 'historical_sessions', availability: 'available' }] }] };
              return api('/api/v1/adapters');
            case 'search_history':
              return api(
                `/api/v1/search?q=${encodeURIComponent(String(args.query))}&limit=${String(args.limit ?? 100)}`,
              );
            case 'export_session':
              return api('/api/v1/exports', {
                method: 'POST',
                body: JSON.stringify({ sessionId: args.sessionId, format: args.format }),
              });
            case 'plan_recovery':
              return api('/api/v1/recovery/plans', {
                method: 'POST',
                body: JSON.stringify(args.request),
              });
            case 'execute_recovery':
              return api(`/api/v1/recovery/plans/${args.planId}/execute`, {
                method: 'POST',
                body: JSON.stringify(args.request),
              });
            case 'import_adapter':
              if (importFixture) { localStorage.setItem('test.import', String(args.adapterId)); return { eventsImported: 7, sources: 1, quarantined: 0 }; }
              return api(`/api/v1/adapters/${args.adapterId}/import`, {
                method: 'POST',
                body: '{}',
              });
            case 'plugin:updater|check':
              return null;
            default:
              throw new Error(`Unhandled test command: ${command}`);
          }
        },
        transformCallback: (callback: (value: unknown) => void) => {
          const id = Math.floor(Math.random() * 1_000_000);
          (window as unknown as Record<string, unknown>)[`_${id}`] = callback;
          return id;
        },
        unregisterCallback: () => undefined,
        convertFileSrc: (value: string) => value,
      };
      (window as unknown as { __TAURI_INTERNALS__: typeof internals }).__TAURI_INTERNALS__ =
        internals;
    },
    { ...harness, importFixture },
  );
}

async function openOnboarded(page: Page) {
  await installBridge(page);
  await page.addInitScript(() => {
    localStorage.setItem('agenttraceback.onboarding.complete', 'true');
    localStorage.removeItem('agenttraceback.onboarding.step');
  });
  await page.goto('/');
}

test('first-run onboarding reaches the real local timeline', async ({ page }) => {
  await installBridge(page);
  await page.goto('/');
  await expect(page.getByRole('heading', { name: /One local history/ })).toBeVisible();
  await page.getByRole('button', { name: /Get started/ }).click();
  await expect(page.getByRole('heading', { name: 'Known agents on this machine' })).toBeVisible();
  await page.getByRole('button', { name: /Continue/ }).click();
  await expect(page.getByRole('heading', { name: 'How capture works' })).toBeVisible();
  await expect(page.getByText('Import existing sessions on the next page or later in Settings.')).toBeVisible();
  await page.getByRole('button', { name: /Continue/ }).click();
  await expect(page.getByRole('heading', { name: /Your local history is ready/ })).toBeVisible();
  await page.getByRole('button', { name: /Open dashboard/ }).click();
  await expect(page.getByRole('heading', { name: 'Live' })).toBeVisible();
  await expect(page.getByText('Demo: verified file write and recovery')).toBeVisible();
});

test('session, evidence, search, findings, and exports use persisted records', async ({
  page,
}) => {
  await openOnboarded(page);
  await expect(page.getByText('Demo: verified file write and recovery')).toBeVisible();

  await page.getByText('Demo: verified file write and recovery').click();
  await page.getByRole('tab', { name: 'Timeline' }).click();
  await expect(page.getByText('VERIFIED').first()).toBeVisible();
  await expect(page.getByText('src/auth.ts').first()).toBeVisible();

  await page.getByRole('button', { name: 'Export redacted report' }).click();
  await expect(page.getByText(/\.md$/)).toBeVisible({ timeout: 15_000 });
  await page.getByRole('button', { name: 'Export full local JSON' }).click();
  await expect(page.getByText(/\.json$/)).toBeVisible({ timeout: 15_000 });

  await page.getByRole('button', { name: 'Search', exact: true }).click();
  await page.getByLabel('Search history').fill('src/auth');
  await expect(page.getByText('src/auth.ts').first()).toBeVisible({ timeout: 15_000 });

  await page.getByRole('button', { name: 'Findings', exact: true }).click();
  await expect(page.getByText('Demo finding: sensitive configuration change')).toBeVisible();
});

test('capture launch screenshots from persisted demo records', async ({ page }) => {
  test.skip(process.env.CAPTURE_SCREENSHOTS !== '1', 'Set CAPTURE_SCREENSHOTS=1 to refresh docs assets.');
  const output = path.resolve(process.cwd(), '../../docs/assets/screenshots');
  fs.mkdirSync(output, { recursive: true });

  await installBridge(page);
  await page.goto('/');
  await page.screenshot({ path: path.join(output, 'onboarding.png'), fullPage: true });
  await page.getByRole('button', { name: /Get started/ }).click();
  await page.getByRole('button', { name: /Continue/ }).click();
  await page.getByRole('button', { name: /Continue/ }).click();
  await page.getByRole('button', { name: /Open dashboard/ }).click();
  await expect(page.getByText('Demo: verified file write and recovery')).toBeVisible();
  await page.screenshot({ path: path.join(output, 'live-dashboard.png'), fullPage: true });

  await page.getByText('Demo: verified file write and recovery').click();
  await page.getByRole('tab', { name: 'Timeline' }).click();
  await expect(page.getByText('VERIFIED').first()).toBeVisible();
  await page.screenshot({ path: path.join(output, 'timeline.png'), fullPage: true });
  await page.locator('.timeline-row').first().click();
  await expect(page.getByRole('complementary', { name: 'Evidence drawer' })).toBeVisible();
  await page.screenshot({ path: path.join(output, 'evidence-drawer.png'), fullPage: true });
  await page.getByRole('button', { name: 'Close evidence drawer' }).click();

  await page.getByRole('tab', { name: 'Files' }).click();
  await expect(page.getByText('src/auth.ts').first()).toBeVisible();
  await page.screenshot({ path: path.join(output, 'file-diff.png'), fullPage: true });

  await page.getByRole('tab', { name: 'Recovery' }).click();
  await page.getByRole('button', { name: 'Plan reconstruction' }).click();
  await expect(page.getByText('1 operations')).toBeVisible({ timeout: 15_000 });
  await page.screenshot({ path: path.join(output, 'recovery.png'), fullPage: true });

  await page.getByRole('button', { name: 'Findings', exact: true }).click();
  await expect(page.getByText('Demo finding: sensitive configuration change')).toBeVisible();
  await page.screenshot({ path: path.join(output, 'finding.png'), fullPage: true });
});


test('recording launcher chooses a folder, reports errors, and passes capture choices', async ({ page }) => {
  await openOnboarded(page);
  await page.getByRole('button', { name: 'Record session', exact: true }).click();
  const launch = page.getByRole('button', { name: 'Start recording in terminal' });
  await expect(launch).toBeDisabled();
  await page.getByRole('button', { name: 'Browse', exact: true }).click();
  await expect(page.getByLabel('Project folder')).toHaveValue('/tmp/project with spaces');
  await page.getByLabel('Agent', { exact: true }).selectOption('claude-code');
  await page.getByLabel('Save an encrypted terminal transcript').uncheck();
  await launch.click();
  await expect(page.getByRole('status').filter({ hasText: 'Launch sent to test terminal' })).toBeVisible();
  expect(await page.evaluate(() => JSON.parse(localStorage.getItem('test.launch') ?? '{}'))).toEqual({ project: '/tmp/project with spaces', agent: 'claude-code', transcript: false });
  await page.getByLabel('Project folder').fill('/missing');
  await launch.click();
  await expect(page.getByRole('alert')).toContainText('Cannot open project folder');
});

test('text menu supports copy, paste, cut, keyboard dismissal and visible command copying', async ({ page }) => {
  await openOnboarded(page);
  await page.getByRole('button', { name: 'Record session', exact: true }).click();
  const folder = page.getByLabel('Project folder');
  await folder.fill('/tmp/example');
  await folder.selectText();
  await folder.click({ button: 'right' });
  await expect(page.getByRole('menu')).toBeVisible();
  await page.getByRole('menuitem', { name: 'Copy', exact: true }).click();
  expect(await page.evaluate(() => localStorage.getItem('test.clipboard'))).toBe('/tmp/example');
  await folder.fill('');
  await folder.click({ button: 'right' });
  await page.getByRole('menuitem', { name: 'Paste', exact: true }).click();
  await expect(folder).toHaveValue('/tmp/example');
  await folder.selectText();
  await folder.click({ button: 'right' });
  await page.getByRole('menuitem', { name: 'Cut', exact: true }).click();
  await expect(folder).toHaveValue('');
  await expect(page.getByRole('button', { name: 'Start recording in terminal' })).toBeDisabled();
  await folder.click({ button: 'right' });
  await page.keyboard.press('Escape');
  await expect(page.getByRole('menu')).toHaveCount(0);
  await expect(folder).toBeFocused();
  await page.getByText('Already using the CLI?', { exact: true }).click();
  await page.getByRole('button', { name: 'Copy', exact: true }).click();
  expect(await page.evaluate(() => localStorage.getItem('test.clipboard'))).toBe('agenttraceback run --agent codex -- codex');
  await expect(page.getByRole('status').filter({ hasText: 'Copied' })).toBeVisible();
});


test('onboarding offers an explicit history import without opening a terminal', async ({ page }) => {
  await installBridge(page, true);
  await page.addInitScript(() => localStorage.setItem('agenttraceback.onboarding.step', '3'));
  await page.goto('/');
  await page.getByRole('button', { name: 'Import Codex history' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Imported 7 events from 1 sources' })).toBeVisible();
  expect(await page.evaluate(() => localStorage.getItem('test.import'))).toBe('codex');
  expect(await page.evaluate(() => localStorage.getItem('test.launch'))).toBeNull();
});
