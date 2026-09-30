import type { Policy } from '../src/gateway.ts'
import assert from 'node:assert/strict'
// eslint-disable-next-line test/no-import-node-test -- Real Vue reactivity with the built-in runner.
import test from 'node:test'
import { computed, reactive } from 'vue'
import { modelChannel, routingModels, setChannel } from '../src/gateway.ts'

function policy(): Policy {
  return reactive({
    enabled: true,
    models: { allow: [], deny: [] },
    accounts: { allow: [], deny: [] },
    model_channels: { model: 'native' },
    key_rules: { 'key-a': { models: { model: 'excel' } } },
    concurrency: 1,
    overflow: 'queue',
    queue_capacity: 32,
    queue_timeout_ms: 120000,
  })
}

test('changing a key override refreshes computed preview and dirty state', () => {
  const p = policy()
  const preview = computed(() => modelChannel(p, 'model', 'model', false, 'key-a'))
  const saved = JSON.stringify(p)
  const dirty = computed(() => JSON.stringify(p) !== saved)
  assert.equal(preview.value, 'excel')
  assert.equal(dirty.value, false)
  setChannel(p, 'model', 'native', 'key-a')
  assert.equal(preview.value, 'native')
  assert.equal(dirty.value, true)
})

test('new key/model rules and inherit cleanup refresh computed catalogs', () => {
  const p = policy()
  const names = computed(() => routingModels(p, [], '-excel'))
  const keys = computed(() => Object.keys(p.key_rules))
  assert.deepEqual(names.value, ['model'])
  assert.deepEqual(keys.value, ['key-a'])
  setChannel(p, 'new-model', 'excel', 'key-b')
  assert.deepEqual(names.value, ['model', 'new-model'])
  assert.deepEqual(keys.value, ['key-a', 'key-b'])
  setChannel(p, 'new-model', 'inherit', 'key-b')
  assert.deepEqual(names.value, ['model'])
  assert.deepEqual(keys.value, ['key-a'])
})
