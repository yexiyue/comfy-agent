// Bounded process readiness and shutdown, shared by initial launch and restart.
import type { ChildProcess } from 'node:child_process';
import { setTimeout as delay } from 'node:timers/promises';

export async function waitUntilHealthy(
  child: ChildProcess,
  api: string,
  log: () => string,
  timeoutMs = 15000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (!child.pid || child.exitCode !== null || child.signalCode !== null)
      throw Error(`Server failed: ${log()}`);
    try {
      if (
        (
          await fetch(new URL('/health', api), {
            signal: AbortSignal.timeout(
              Math.min(1000, Math.max(1, deadline - Date.now())),
            ),
          })
        ).ok
      )
        return;
    } catch {}
    await delay(100);
  }
  throw Error(`Server health timeout: ${log()}`);
}

export async function stopServer(
  child: ChildProcess,
  force: boolean,
  log: () => string,
): Promise<void> {
  if (!child.pid || child.exitCode !== null || child.signalCode !== null)
    return;
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => {
      child.kill();
      reject(Error('Server shutdown timed out'));
    }, 12000);
    child.once('exit', (code) => {
      clearTimeout(timer);
      if (force || code === 0) resolve();
      else reject(Error(`Server shutdown failed (${code}): ${log()}`));
    });
    if (force) child.kill();
    else child.stdin!.write('shutdown\n');
  });
}
