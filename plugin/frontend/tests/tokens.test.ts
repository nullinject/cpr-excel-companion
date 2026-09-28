import assert from 'node:assert/strict'
// eslint-disable-next-line test/no-import-node-test -- Use the built-in runner; no test framework dependency.
import test from 'node:test'
import { formatTokens, tokensPerSecond } from '../src/gateway.ts'

test('unknown usage stays unknown without breaking monitor rendering', () => {
  assert.equal(formatTokens(null), '—')
  assert.equal(formatTokens(undefined), '—')
  assert.equal(formatTokens(0), '0')
  assert.equal(formatTokens(1234), '1,234')
})

test('generation rate uses measured first-token and completion timings', () => {
  assert.equal(tokensPerSecond({ output_tokens: 70, timings: { first_token_ms: 1000, latency_ms: 3000 } }), 35)
  assert.equal(tokensPerSecond({ output_tokens: 0, timings: { first_token_ms: 1000, latency_ms: 3000 } }), 0)
  assert.equal(tokensPerSecond({ output_tokens: null, timings: { first_token_ms: 1000, latency_ms: 3000 } }), null)
  assert.equal(tokensPerSecond({ output_tokens: 70 }), null)
  assert.equal(tokensPerSecond(null), null)
  assert.equal(tokensPerSecond({ output_tokens: 70, timings: { first_token_ms: 1000, latency_ms: 1000 } }), null)
})
