<script setup lang="ts">
import { BaseButton, BaseCard } from '@codex-proxy/ui'
import { onMounted, onUnmounted, ref } from 'vue'

interface Scope { allow: string[], deny: string[] }
interface Policy { enabled: boolean, models: Scope, accounts: Scope, client_keys: Scope, concurrency: number, overflow: 'queue' | 'reject', queue_capacity: number, queue_timeout_ms: number }
interface Row { request_id: string, model: string, client_key_id: string | null, account_id: string | null, status: string, started_at_ms: number, queue_ms: number | null, finished_at_ms: number | null, usage: { input_tokens?: number, output_tokens?: number, cost?: unknown } | null, error: string | null, error_code: string | null, upstream_model: string | null, source: string }
interface Snapshot { policy: Policy, version: number | null, active: number, waiting: number, records: Row[] }
const snapshot = ref<Snapshot | null>(null)
const policy = ref<Policy | null>(null)
const version = ref<number | null>(null)
const error = ref('')
const success = ref('')
const saving = ref(false)
const tab = ref<'monitor' | 'settings'>('monitor')
const filter = ref('')
const options = ref<{ accounts: { account_id: string, enabled: boolean }[], keys: { id: string, name: string, enabled: boolean }[] }>({ accounts: [], keys: [] })
function choices(scope: typeof scopes[number]) {
  if (scope === 'accounts')
    return options.value.accounts.map(account => ({ id: account.account_id, label: `${account.account_id}${account.enabled ? '' : '（停用）'}` }))
  if (scope === 'client_keys')
    return options.value.keys.map(key => ({ id: key.id, label: `${key.name} · ${key.id}${key.enabled ? '' : '（停用）'}` }))
  return []
}
function addChoice(scope: typeof scopes[number], mode: 'allow' | 'deny', event: Event) {
  const select = event.target as HTMLSelectElement
  if (policy.value && select.value && !policy.value[scope][mode].includes(select.value))
    policy.value[scope][mode].push(select.value)
  select.value = ''
}
async function loadOptions() {
  try {
    options.value = await api('api/options')
  }
  catch (cause) { error.value = cause instanceof Error ? cause.message : '读取可选账户和 Key 失败' }
}
const scopes = ['models', 'accounts', 'client_keys'] as const
const scopeNames = { models: '模型', accounts: '账户 ID', client_keys: 'Client Key ID' }
let timer: ReturnType<typeof setInterval> | undefined
let fetching = false
async function api(path: string, data?: unknown) {
  const host = (window as unknown as { codexProxyPlugin?: { version: number, request: (r: unknown) => Promise<{ status: number, body: ArrayBuffer }> } }).codexProxyPlugin
  if (!host || host.version !== 2)
    throw new Error('请从 CPR 的 Excel 网关侧边栏打开')
  const response = await host.request({ path, method: data === undefined ? 'GET' : 'POST', ...(data === undefined ? {} : { contentType: 'application/json', body: JSON.stringify(data) }) })
  const value = JSON.parse(new TextDecoder().decode(response.body))
  if (response.status >= 400)
    throw new Error(typeof value.error === 'string' ? value.error : '操作未完成')
  return value
}
async function refresh(loadForm = false) {
  if (fetching)
    return
  fetching = true
  try {
    const data = await api('api/snapshot') as Snapshot
    snapshot.value = data
    if (loadForm || !policy.value) {
      policy.value = structuredClone(data.policy)
      version.value = data.version
    }
    error.value = ''
  }
  catch (cause) { error.value = cause instanceof Error ? cause.message : '读取失败' }
  finally { fetching = false }
}
function updateScope(scope: typeof scopes[number], mode: 'allow' | 'deny', event: Event) {
  if (policy.value)
    policy.value[scope][mode] = [...new Set((event.target as HTMLTextAreaElement).value.split(/[\n,]+/).map(s => s.trim()).filter(Boolean))]
}
async function save() {
  saving.value = true
  success.value = ''
  try {
    await api('api/policy', { policy: policy.value, expected_version: version.value })
    await refresh(true)
    success.value = '配置已保存'
  }
  catch (cause) { error.value = cause instanceof Error ? cause.message : '保存失败' }
  finally { saving.value = false }
}
function rows() {
  return snapshot.value?.records.filter(row => !filter.value || `${row.model} ${row.request_id} ${row.status} ${row.client_key_id ?? ''} ${row.error ?? ''} ${row.error_code ?? ''} ${row.upstream_model ?? ''}`.includes(filter.value)) ?? []
}
onMounted(() => {
  void refresh(true)
  timer = setInterval(() => {
    if (tab.value === 'monitor')
      void refresh()
  }, 5000)
})
onUnmounted(() => {
  if (timer)
    clearInterval(timer)
})
</script>

<template>
  <main class="gateway">
    <nav aria-label="Excel 网关页面">
      <BaseButton @click="tab = 'monitor'">
        请求监控
      </BaseButton>
      <BaseButton @click="tab = 'settings'; loadOptions()">
        网关设置
      </BaseButton>
      <span v-if="snapshot" class="summary">运行 {{ snapshot.active }} · 排队 {{ snapshot.waiting }} · {{ snapshot.policy.enabled ? '已开启' : '已关闭' }}</span>
      <BaseButton @click="refresh(tab === 'settings')">
        刷新
      </BaseButton>
    </nav>
    <p v-if="error" class="error" role="alert">
      {{ error }}
    </p>
    <p v-if="success" role="status">
      {{ success }}
    </p>
    <BaseCard v-if="tab === 'monitor'">
      <div class="toolbar">
        <label for="filter">筛选 <input id="filter" v-model="filter" placeholder="模型、请求 ID、状态或 Key ID"></label><small>最近 1000 条 · 进程重启后清空</small>
      </div>
      <div class="table-scroll">
        <table>
          <thead><tr><th>时间</th><th>模型 / 请求</th><th>状态</th><th>账户 / Key</th><th>排队</th><th>耗时</th><th>输入 / 输出 tokens</th><th>上游模型</th><th>错误</th></tr></thead>
          <tbody>
            <tr v-for="row in rows()" :key="row.request_id">
              <td>{{ new Date(row.started_at_ms).toLocaleTimeString() }}</td>
              <td>{{ row.model }}<small>{{ row.request_id }}</small></td>
              <td>{{ row.status }}<small v-if="row.source === 'host'">宿主观察</small><small v-else-if="row.source === 'unsigned'">未签名透传</small></td>
              <td>{{ row.account_id ?? '—' }}<small>{{ row.client_key_id ?? '—' }}</small></td>
              <td>{{ row.queue_ms ?? '—' }} ms</td>
              <td>{{ row.finished_at_ms == null ? '—' : row.finished_at_ms - row.started_at_ms }} ms</td>
              <td>{{ row.usage?.input_tokens ?? '—' }} / {{ row.usage?.output_tokens ?? '—' }}</td>
              <td>{{ row.upstream_model ?? '—' }}</td>
              <td class="error-cell">{{ row.error || row.error_code || '—' }}</td>
            </tr><tr v-if="rows().length === 0">
              <td colspan="9" class="empty">
                暂无匹配的 Excel 请求
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </BaseCard>
    <form v-if="tab === 'settings' && policy" @submit.prevent="save">
      <BaseCard>
        <p>按 Key × 模型切换：在 CPR 插件实例的「绑定」里配置 clientKeyIds 与 models——命中的请求由本插件签名走 Excel（excelMode=suffix 时由客户端模型名后缀决定，always/never 固定），未命中的 Key 由桥接自动原生透传。此页的模型/账户/Key 范围是桥接侧的准入门（并发与队列），黑名单优先，白名单留空表示不限。</p>
        <div class="settings">
          <label for="enabled"> <input id="enabled" v-model="policy.enabled" type="checkbox"> 开启 Excel 网关</label>
          <label for="concurrency">最大并发 <input id="concurrency" v-model.number="policy.concurrency" type="number" min="1" max="1024" required></label>
          <label for="overflow">超出并发时 <select id="overflow" v-model="policy.overflow"><option value="queue">按 FIFO 排队</option><option value="reject">直接拒绝</option></select></label>
          <label v-if="policy.overflow === 'queue'" for="capacity">队列上限 <input id="capacity" v-model.number="policy.queue_capacity" type="number" min="0" max="10000" required></label>
          <label v-if="policy.overflow === 'queue'" for="timeout">排队超时（毫秒） <input id="timeout" v-model.number="policy.queue_timeout_ms" type="number" min="1" max="600000" required></label>
        </div>
      </BaseCard>
      <BaseCard>
        <p>黑名单优先，白名单留空表示不限，每行一个完整名称或 ID</p>
        <div v-for="scope in scopes" :key="scope" class="scope">
          <strong>{{ scopeNames[scope] }}</strong>
          <label :for="`${scope}-allow`">
            白名单
            <select v-if="scope !== 'models'" :aria-label="`${scopeNames[scope]}加入白名单`" @change="addChoice(scope, 'allow', $event)">
              <option value="">选择并添加</option><option v-for="option in choices(scope)" :key="option.id" :value="option.id">{{ option.label }}</option>
            </select>
            <textarea :id="`${scope}-allow`" :value="policy[scope].allow.join('\n')" rows="3" @change="updateScope(scope, 'allow', $event)" />
          </label>
          <label :for="`${scope}-deny`">
            黑名单
            <select v-if="scope !== 'models'" :aria-label="`${scopeNames[scope]}加入黑名单`" @change="addChoice(scope, 'deny', $event)">
              <option value="">选择并添加</option><option v-for="option in choices(scope)" :key="option.id" :value="option.id">{{ option.label }}</option>
            </select>
            <textarea :id="`${scope}-deny`" :value="policy[scope].deny.join('\n')" rows="3" @change="updateScope(scope, 'deny', $event)" />
          </label>
        </div>
      </BaseCard>
      <div class="toolbar">
        <BaseButton type="submit" :disabled="saving">
          {{ saving ? '保存中' : '保存配置' }}
        </BaseButton><small>有运行或排队请求时需等待结束后保存</small>
      </div>
    </form>
  </main>
</template>

<style>
body {
  margin: 0;
}
.gateway {
  font-family: var(--cp-font-family, sans-serif);
  color: var(--cp-color-text);
  display: flex;
  flex-direction: column;
  gap: 16px;
  padding: 16px;
  font-size: 14px;
}
nav,
.toolbar {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}
.summary {
  margin-left: auto;
}
small {
  display: block;
  color: var(--cp-color-text-secondary);
  font-size: 12px;
}
input,
select,
textarea {
  font: inherit;
  color: inherit;
  background: var(--cp-color-bg-container, transparent);
  border: 1px solid var(--cp-color-border, #8886);
  border-radius: 6px;
  padding: 7px;
}
input[type='checkbox'] {
  width: auto;
}
input[type='number'] {
  width: 100px;
}
label {
  display: flex;
  align-items: center;
  gap: 8px;
}
form {
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.settings {
  display: flex;
  gap: 20px;
  flex-wrap: wrap;
}
.scope {
  display: grid;
  grid-template-columns: 120px 1fr 1fr;
  gap: 16px;
  padding: 12px 0;
}
.scope label {
  align-items: stretch;
  flex-direction: column;
}
.table-scroll {
  overflow: auto;
}
table {
  width: 100%;
  border-collapse: collapse;
  text-align: left;
  white-space: nowrap;
  margin-top: 16px;
}
td,
th {
  padding: 10px;
  border-bottom: 1px solid var(--cp-color-border, #8883);
}
th {
  font-weight: 500;
  color: var(--cp-color-text-secondary);
}
.empty {
  text-align: center;
  padding: 40px;
}
.error {
  color: var(--cp-color-error, #b42318);
}
.error-cell {
  max-width: 280px;
  overflow: hidden;
  text-overflow: ellipsis;
}
@media (max-width: 700px) {
  .scope {
    grid-template-columns: 1fr;
  }
  .summary {
    margin-left: 0;
  }
  label {
    flex-wrap: wrap;
  }
}
</style>
