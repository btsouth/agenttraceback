import { spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

export const HARNESS_PATH = path.join(os.tmpdir(), 'agenttraceback-playwright-harness.json');

export default async function globalSetup() {
  const root = path.resolve(process.cwd(), '../..');
  const daemon = path.join(root, 'target/debug/agenttracebackd');
  { // Build even when an older daemon binary exists; the bridge must test current code.
    const build = spawnSync('cargo', ['build', '-p', 'agenttraceback-daemon'], {
      cwd: root,
      stdio: 'inherit',
    });
    if (build.status !== 0 || !fs.existsSync(daemon)) {
      throw new Error(`Could not build ${daemon}.`);
    }
  }
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'agenttraceback-e2e.'));
  const home = path.join(temporary, 'home');
  fs.mkdirSync(path.join(home, '.claude/projects'), { recursive: true });
  const history = Array.from({ length: 1501 }, (_, index) => JSON.stringify({ type: 'user', sessionId: 'onboarding-history', uuid: `fixture-${index}`, timestamp: '2026-09-20T10:00:00Z', message: { role: 'user', content: `History fixture ${index}` } })).join('\n') + '\n';
  fs.writeFileSync(path.join(home, '.claude/projects/history.jsonl'), history);
  const data = path.join(temporary, 'data');
  const config = path.join(temporary, 'config');
  const runtime = path.join(temporary, 'runtime');
  fs.mkdirSync(runtime, { recursive: true });
  const child = spawn(daemon, [], {
    cwd: root,
    detached: true,
    stdio: 'ignore',
    env: {
      ...process.env,
      HOME: home,
      USERPROFILE: home,
      PATH: path.dirname(daemon),
      AGENTTRACEBACK_DATA_DIR: data,
      AGENTTRACEBACK_CONFIG_DIR: config,
      XDG_RUNTIME_DIR: runtime,
    },
  });
  child.unref();

  const runtimeFile = path.join(runtime, 'agenttraceback/runtime.json');
  let metadata: { pid: number; port: number; token: string } | undefined;
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (fs.existsSync(runtimeFile)) {
      metadata = JSON.parse(fs.readFileSync(runtimeFile, 'utf8')) as typeof metadata;
      break;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  if (!metadata) {
    throw new Error('Daemon did not publish runtime metadata.');
  }
  const baseUrl = `http://127.0.0.1:${metadata.port}`;
  const response = await fetch(`${baseUrl}/api/v1/demo/install`, {
    method: 'POST',
    headers: {
      authorization: `Bearer ${metadata.token}`,
      'content-type': 'application/json',
    },
    body: '{}',
  });
  if (!response.ok) {
    throw new Error(`Demo install failed: ${response.status} ${await response.text()}`);
  }
  fs.writeFileSync(
    HARNESS_PATH,
    JSON.stringify({ ...metadata, baseUrl, temporary }, null, 2),
  );
}
