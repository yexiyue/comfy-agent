import { createServer } from 'node:http';
import { createMockModel } from './mock-model.ts';
import { waitUntilHealthy, stopServer } from './server-process.ts';
import type { ChildProcess } from 'node:child_process';
import { spawn, spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { mkdir, copyFile, unlink } from 'node:fs/promises';
const root = fileURLToPath(new URL('../../../', import.meta.url));
export async function startMock(otel = false, maxSteps = 6, real = false) {
  let database = process.env.TEST_DATABASE_URL;
  if (!real && !database?.endsWith('_test'))
    throw Error(
      'Mock eval requires dedicated TEST_DATABASE_URL ending in _test',
    );
  const modelCalls = new Map<string, number>();
  const mock = createMockModel((prompt, count) => modelCalls.set(prompt, count));
  let child: ChildProcess | undefined;
  let runExecutable: string | undefined;
  let fixtureDatabase: string | undefined;
  let log = '';
  let closing: Promise<void> | undefined;

  async function cleanup(force = false): Promise<void> {
    const errors: unknown[] = [];
    try {
      if (child) await stopServer(child, force, () => log);
    } catch (error) {
      errors.push(error);
    }
    mock.closeAllConnections();
    if (mock.listening)
      await new Promise<void>((resolve) => mock.close(() => resolve()));
    if (runExecutable) {
      try {
        await unlink(runExecutable);
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'ENOENT')
          errors.push(error);
      }
    }
    if (fixtureDatabase) {
      const dropped = spawnSync(
        'cargo',
        [
          'run',
          '--quiet',
          '-p',
          'persistence',
          '--bin',
          'test_database',
          '--',
          'drop',
          fixtureDatabase,
        ],
        { cwd: root, env: process.env, encoding: 'utf8', windowsHide: true },
      );
      if (dropped.status !== 0)
        errors.push(Error('Fixture database cleanup failed'));
    }
    if (errors.length)
      throw new AggregateError(errors, 'Fixture cleanup failed');
  }
  function close(): Promise<void> {
    return (closing ??= cleanup());
  }

  try {
    await new Promise<void>((r) => mock.listen(0, '127.0.0.1', r));
    const address = mock.address();
    if (!address || typeof address === 'string') throw Error('No mock port');
    const probe = createServer();
    await new Promise<void>((r) => probe.listen(0, '127.0.0.1', r));
    const pa = probe.address();
    if (!pa || typeof pa === 'string') throw Error('No server port');
    await new Promise<void>((r) => probe.close(() => r()));
    await mkdir(`${root}/outputs`, { recursive: true });
    const manifest = `${root}/outputs/mock-config-${pa.port}.json`;
    const executable = process.platform === 'win32' ? 'server.exe' : 'server';
    runExecutable = `${root}/outputs/eval-server-${pa.port}${process.platform === 'win32' ? '.exe' : ''}`;
    await copyFile(`${root}/target/debug/${executable}`, runExecutable);
    const modelEnv = real
      ? {}
      : {
          MODEL: 'openai::gpt-4.1',
          OPENAI_API_KEY: 'mock-key',
          API_BASE_URL: `http://127.0.0.1:${address.port}/v1/`,
        };
    if (!real) {
      const created = spawnSync(
        'cargo',
        [
          'run',
          '--quiet',
          '-p',
          'persistence',
          '--bin',
          'test_database',
          '--',
          'create',
        ],
        { cwd: root, env: process.env, encoding: 'utf8', windowsHide: true },
      );
      if (created.status !== 0)
        throw Error('Test database provisioning failed');
      fixtureDatabase = created.stdout.trim();
      if (!/^agent_fixture_[a-f0-9]{32}_test$/.test(fixtureDatabase))
        throw Error('Invalid fixture database name');
      database =
        database!.slice(0, database!.lastIndexOf('/') + 1) + fixtureDatabase;
      const migrated = spawnSync(
        'cargo',
        ['run', '--quiet', '-p', 'persistence', '--bin', 'migrate'],
        {
          cwd: root,
          env: { ...process.env, DATABASE_URL: database },
          encoding: 'utf8',
          windowsHide: true,
        },
      );
      if (migrated.status !== 0) throw Error('Fixture migration failed');
    }
    const environment = {
      ...process.env,
      ...modelEnv,
      ...(!real ? { DATABASE_URL: database } : {}),
      SERVER_ADDR: `127.0.0.1:${pa.port}`,
      AGENT_MAX_STEPS: String(maxSteps),
      OTEL_ENABLED: String(otel),
      AGENT_CONFIG_MANIFEST: manifest,
      SERVER_SHUTDOWN_STDIN: 'true',
      RUST_LOG: 'info',
      RUN_LEASE_SECONDS: process.env.RUN_LEASE_SECONDS ?? '3',
      RUN_HEARTBEAT_SECONDS: process.env.RUN_HEARTBEAT_SECONDS ?? '1',
    };
    const launch = () =>
      spawn(runExecutable!, [], {
        cwd: root,
        env: environment,
        stdio: ['pipe', 'pipe', 'pipe'],
        windowsHide: true,
      });
    function launchServer(): ChildProcess {
      const process = launch();
      process.stdout!.on('data', (chunk) => {
        log += String(chunk);
      });
      process.stderr!.on('data', (chunk) => {
        log += String(chunk);
      });
      process.on('error', (error) => {
        log += error.message;
      });
      return process;
    }
    child = launchServer();
    const api = `http://127.0.0.1:${pa.port}/api/chat`;
    await waitUntilHealthy(child, api, () => log);
    return {
      api,
      manifest,
      log: () => log,
      waitForModel: async (prompt: string, count = 1, timeoutMs = 5000) => {
        const deadline = Date.now() + timeoutMs;
        while ((modelCalls.get(prompt) ?? 0) < count) {
          if (Date.now() >= deadline) throw Error('Mock model did not receive the expected request');
          await new Promise(resolve => setTimeout(resolve, 10));
        }
      },
      get child() {
        return child!;
      },
      restart: async (force = true) => {
        await stopServer(child!, force, () => log);
        child = launchServer();
        await waitUntilHealthy(child, api, () => log);
      },
      close,
    };
  } catch (error) {
    try {
      await cleanup(true);
    } catch (cleanupError) {
      throw new AggregateError(
        [error, cleanupError],
        'Fixture startup and cleanup failed',
      );
    }
    throw error;
  }
}
