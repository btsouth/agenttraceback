import fs from 'node:fs';

import { HARNESS_PATH } from './global-setup';

export default async function globalTeardown() {
  if (!fs.existsSync(HARNESS_PATH)) return;
  const harness = JSON.parse(fs.readFileSync(HARNESS_PATH, 'utf8')) as {
    pid: number;
    temporary: string;
  };
  try {
    process.kill(harness.pid, 'SIGTERM');
  } catch {
    // The daemon may already have exited after a failed test.
  }
  fs.rmSync(harness.temporary, { recursive: true, force: true });
  fs.rmSync(HARNESS_PATH, { force: true });
}
