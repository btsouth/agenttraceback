import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { expect, test, type Page } from '@playwright/test';

const harness = JSON.parse(
  fs.readFileSync(path.join(os.tmpdir(), 'agenttraceback-playwright-harness.json'), 'utf8'),
) as { baseUrl: string; token: string };

async function installBridge(page: Page) {
  await page.addInitScript(
    ({ baseUrl, token }) => {
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
    harness,
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
  await page.getByRole('button', { name: /Continue/ }).click();
  await expect(page.getByRole('heading', { name: /Your local history is ready/ })).toBeVisible();
  await page.getByRole('button', { name: /Explore imported sessions/ }).click();
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
  await page.getByRole('button', { name: /Explore imported sessions/ }).click();
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
