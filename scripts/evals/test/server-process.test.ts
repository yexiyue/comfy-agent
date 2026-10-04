import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { waitUntilHealthy, stopServer } from '../src/server-process.js';

test('readiness timeout fails instead of returning an unready fixture', async () => {
  const http = createServer((_request, response) => {
    response.writeHead(503);
    response.end();
  });
  await new Promise<void>((resolve) => http.listen(0, '127.0.0.1', resolve));
  const address = http.address();
  assert(address && typeof address !== 'string');
  const child = spawn(process.execPath, ['-e', 'setInterval(()=>{},1000)'], {
    stdio: ['pipe', 'ignore', 'ignore'],
    windowsHide: true,
  });
  try {
    await assert.rejects(
      waitUntilHealthy(
        child,
        `http://127.0.0.1:${address.port}/api/chat`,
        () => '',
        200,
      ),
      /health timeout/,
    );
  } finally {
    await stopServer(child, true, () => '');
    await new Promise<void>((resolve) => http.close(() => resolve()));
  }
  assert(child.exitCode !== null || child.signalCode !== null);
});
