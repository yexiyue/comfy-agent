import { test } from 'node:test'
import assert from 'node:assert/strict'
import { SessionRequests } from '../src/lib/session-requests.ts'

function deferred() {
  let resolve
  const promise = new Promise((done) => {
    resolve = done
  })
  return { promise, resolve }
}

test('a delayed A poll cannot publish after selecting B, even when fetch ignores abort', async () => {
  const requests = new SessionRequests()
  requests.finish(requests.begin())
  const polling = requests.capture()
  const result = deferred()
  let selectedRun = 'A'
  const pending = result.promise.then((run) => {
    if (requests.isCurrent(polling)) selectedRun = run
  })
  const selection = requests.begin()
  selectedRun = 'B'
  requests.finish(selection)
  result.resolve('A')
  await pending
  assert.equal(selectedRun, 'B')
  assert(polling.signal.aborted)
})

test('latest selection wins while older load is still pending', async () => {
  const requests = new SessionRequests()
  const a = requests.begin()
  const deferredA = deferred()
  let selected = null
  const pendingA = deferredA.promise.then((value) => {
    if (requests.isCurrent(a)) selected = value
    requests.finish(a)
  })
  const b = requests.begin()
  selected = 'B'
  assert(requests.finish(b))
  deferredA.resolve('A')
  await pendingA
  assert.equal(selected, 'B')
  assert.equal(requests.loading, false)
  requests.dispose()
  assert(!requests.isCurrent(b))
})
