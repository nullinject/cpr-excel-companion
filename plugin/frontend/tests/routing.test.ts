import type { Policy, RequestRow } from '../src/gateway.ts'
import assert from 'node:assert/strict'
// eslint-disable-next-line test/no-import-node-test -- Built-in runner, no extra test framework.
import test from 'node:test'
import { modelChannel, routingModels, setChannel } from '../src/gateway.ts'

function policy(): Policy {
  return {
    enabled: true,
    models: { allow: [], deny: [] },
    accounts: { allow: [], deny: [] },
    model_channels: { model: 'native' },
    key_rules: {},
    concurrency: 1,
    overflow: 'queue',
    queue_capacity: 32,
    queue_timeout_ms: 120000,
  }
}

test('key model routing is isolated and overrides global and suffix', () => {
  const p = policy()
  setChannel(p, 'model', 'excel', 'key-a')
  setChannel(p, 'model', 'native', 'key-b')
  assert.equal(modelChannel(p, 'model', 'model', false, 'key-a'), 'excel')
  assert.equal(modelChannel(p, 'model', 'model-excel', true, 'key-b'), 'native')
  assert.equal(modelChannel(p, 'model', 'model-excel', true, 'unknown'), 'native')
  assert.deepEqual(p.model_channels, { model: 'native' })
})

test('inherit removes empty key rules and resumes global routing', () => {
  const p = policy()
  setChannel(p, 'model', 'excel', 'key-a')
  setChannel(p, 'model', 'inherit', 'key-a')
  assert.deepEqual(p.key_rules, {})
  assert.equal(modelChannel(p, 'model', 'model-excel', true, 'key-a'), 'native')
  setChannel(p, 'model', 'inherit')
  assert.equal(modelChannel(p, 'model', 'model-excel', true, 'key-a'), 'excel')
})

test('route preview never bypasses disabled channel or model denial', () => {
  const p = policy()
  setChannel(p, 'model', 'excel', 'key-a')
  p.enabled = false
  assert.equal(modelChannel(p, 'model', 'model', false, 'key-a'), 'native')
  p.enabled = true
  p.models.deny.push('model')
  assert.equal(modelChannel(p, 'model', 'model', false, 'key-a'), 'native')
})

test('dictionary-like model and key identifiers cannot mutate prototypes', () => {
  const p = policy()
  setChannel(p, '__proto__', 'excel', '__proto__')
  assert.equal(Object.hasOwn(p.key_rules, '__proto__'), true)
  assert.equal(modelChannel(p, '__proto__', '__proto__', false, '__proto__'), 'excel')
  assert.equal(Object.getPrototypeOf(p.key_rules), Object.prototype)
  assert.equal(Object.hasOwn(Object.prototype, 'models'), false)
  setChannel(p, '__proto__', 'inherit', '__proto__')
  assert.deepEqual(p.key_rules, {})
})

test('model choices retain key-only rules and observations without fabricating availability', () => {
  const p = policy()
  setChannel(p, 'key-only-model', 'excel', 'missing-key')
  const rows = [{ model: 'observed-excel' }, { model: 'observed' }] as RequestRow[]
  assert.deepEqual(routingModels(p, rows, '-excel'), ['key-only-model', 'model', 'observed'])
  assert.deepEqual(routingModels(null, [], '-excel'), [])
})
