import { spawnSync } from 'node:child_process'
import { mkdtemp, readdir, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { basename, dirname, join, relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { createClient } from '@hey-api/openapi-ts'
import configuration from '../openapi-ts.config.ts'

const web = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const root = resolve(web, '../..')
const check = process.argv.includes('--check')
const scratch = check ? await mkdtemp(join(tmpdir(), 'comfy-api-')) : null
const spec = scratch
  ? join(scratch, 'openapi.json')
  : join(root, 'docs/api/openapi.json')
const output = scratch
  ? join(scratch, 'client')
  : join(web, 'src/api/generated')

function run(command, args) {
  const result = spawnSync(command, args, {
    cwd: web,
    stdio: 'inherit',
    windowsHide: true,
  })
  if (result.error) throw result.error
  if (result.status !== 0)
    throw Error(`API generation failed (${result.status})`)
}
async function files(path) {
  const entries = await readdir(path, { withFileTypes: true })
  const groups = await Promise.all(
    entries.map((entry) =>
      entry.isDirectory()
        ? files(join(path, entry.name))
        : [join(path, entry.name)],
    ),
  )
  return groups.flat()
}
async function text(path) {
  return (await readFile(path, 'utf8')).replaceAll('\r\n', '\n')
}

try {
  run('cargo', [
    'run',
    '--quiet',
    '--manifest-path',
    join(root, 'Cargo.toml'),
    '-p',
    'server',
    '--bin',
    'export_openapi',
    '--',
    spec,
  ])
  await createClient({
    ...configuration,
    input: spec,
    output: { ...configuration.output, path: output },
  })
  if (check) {
    const storedOutput = join(web, 'src/api/generated')
    const generated = (await files(output))
      .map((path) => relative(output, path))
      .sort()
    const stored = (await files(storedOutput))
      .map((path) => relative(storedOutput, path))
      .sort()
    const stale = []
    if (
      (await text(spec)) !== (await text(join(root, 'docs/api/openapi.json')))
    )
      stale.push('docs/api/openapi.json')
    if (JSON.stringify(generated) !== JSON.stringify(stored))
      stale.push('generated file list')
    for (const name of generated) {
      if (
        !stored.includes(name) ||
        (await text(join(output, name))) !==
          (await text(join(storedOutput, name)))
      )
        stale.push(name)
    }
    if (stale.length)
      throw Error(
        `Stale API artifacts: ${stale.join(', ')}. Run pnpm -C apps/web api:generate`,
      )
    console.log('OpenAPI and generated client are current')
  }
} finally {
  if (scratch) {
    const target = resolve(scratch)
    if (
      dirname(target) !== resolve(tmpdir()) ||
      !basename(target).startsWith('comfy-api-')
    ) {
      throw Error('Refusing to remove an unexpected generation directory')
    }
    await rm(target, { recursive: true, force: true })
  }
}
