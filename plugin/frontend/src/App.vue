<script setup lang="ts">
import { BaseButton, BaseCard } from '@codex-proxy/ui'
import { computed, onMounted, onUnmounted, ref } from 'vue'

interface Scope { allow: string[], deny: string[] }
interface Policy {
  enabled: boolean
  models: Scope
  accounts: Scope
  client_keys: Scope
  model_channels: Record<string, 'excel' | 'native'>
  concurrency: number
  overflow: 'queue' | 'reject'
  queue_capacity: number
  queue_timeout_ms: number
}
interface Row {
  request_id: string
  model: string
  client_key_id: string | null
  account_id: string | null
  status: string
  started_at_ms: number
  queue_ms: number | null
  finished_at_ms: number | null
  usage: { input_tokens?: number, output_tokens?: number } | null
  error: string | null
  error_code: string | null
  upstream_model: string | null
  source: string
  excel: boolean
}
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
const pluginInfo = ref<{ excelEnabled: boolean, excelMode: string, excelModelSuffix: string, isolationScope: string } | null>(null)
const newModel = ref('')

const keyNames = computed(() => {
  const map = new Map<string, string>()
  for (const key of options.value.keys)
    map.set(key.id, key.name)
  return map
})
function keyLabel(id: string | null): string {
  if (!id || id === 'unsigned')
    return '未签名'
  return keyNames.value.get(id) ?? id.slice(0, 14) + '…'
}
function baseName(model: string): string {
  const suffix = pluginInfo.value?.excelModelSuffix ?? '-excel'
  return model.endsWith(suffix) && model.length > suffix.length ? model.slice(0, -suffix.length) : model
}
function modelState(model: string): 'suffix' | 'excel' | 'native' {
  return policy.value?.model_channels[model] ?? 'suffix'
}
function setModelState(model: string, state: 'suffix' | 'excel' | 'native') {
  const p = policy.value
  if (!p) return
  if (state === 'suffix')
    delete p.model_channels[model]
  else
    p.model_channels[model] = state
}
const knownModels = computed(() => {
  const set = new Set<string>()
  for (const row of snapshot.value?.records ?? []) {
    if (row.model)
      set.add(baseName(row.model))
  }
  for (const m of Object.keys(policy.value?.model_channels ?? {})) set.add(m)
  return [...set].sort()
})
const accountList = computed(() => {
  const map = new Map<string, boolean>()
  for (const a of options.value.accounts) map.set(a.account_id, a.enabled)
  for (const id of policy.value?.accounts.allow ?? []) if (!map.has(id)) map.set(id, true)
  for (const id of policy.value?.accounts.deny ?? []) if (!map.has(id)) map.set(id, true)
  return [...map.entries()].map(([id, enabled]) => ({ id, enabled }))
})
function channelTextOf(model: string): string {
  const state = modelState(model)
  return state === 'excel' ? 'Excel' : state === 'native' ? '原生' : '跟随后缀'
}
function accountAllowed(id: string): boolean {
  const p = policy.value
  if (!p) return true
  if (p.accounts.deny.includes(id)) return false
  if (p.accounts.allow.length > 0 && !p.accounts.allow.includes(id)) return false
  return true
}
function setAccountAllowed(id: string, allowed: boolean) {
  const p = policy.value
  if (!p) return
  p.accounts.deny = p.accounts.deny.filter(a => a !== id)
  if (allowed) {
    if (p.accounts.allow.length > 0)
      p.accounts.allow = [...new Set([...p.accounts.allow, id])]
  } else {
    p.accounts.allow = p.accounts.allow.filter(a => a !== id)
    p.accounts.deny.push(id)
  }
}

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
}
async function loadOptions() {
  try {
    options.value = await api('api/options')
  }
  catch { /* 选项加载失败不阻塞页面，Key 显示退回 ID */ }
  try {
    pluginInfo.value = await api('api/plugin-info')
  }
  catch { /* 同上 */ }
}
const stats = computed(() => {
  const rows = snapshot.value?.records ?? []
  let excel = 0
  let excelFailed = 0
  let passthrough = 0
  let native = 0
  for (const row of rows) {
    if (row.excel) {
      if (row.status === 'succeeded' || row.status === 'completed')
        excel += 1
      else
        excelFailed += 1
    } else if (row.source === 'unsigned') {
      passthrough += 1
    } else if (row.status === 'succeeded' || row.status === 'completed') {
      native += 1
    }
  }
  return { excel, excelFailed, passthrough, native, total: rows.length }
})
const learnedKeys = computed(() => {
  const counts = new Map<string, { excel: number, native: number }>()
  for (const row of snapshot.value?.records ?? []) {
    if (!row.client_key_id || row.client_key_id === 'unsigned')
      continue
    const entry = counts.get(row.client_key_id) ?? { excel: 0, native: 0 }
    if (row.excel)
      entry.excel += 1
    else
      entry.native += 1
    counts.set(row.client_key_id, entry)
  }
  return [...counts.entries()].map(([id, c]) => ({ id, name: keyNames.value.get(id) ?? id.slice(0, 14) + '…', ...c })).sort((a, b) => b.excel + b.native - (a.excel + a.native))
})
function rows() {
  return snapshot.value?.records.filter((row) => {
    if (!filter.value) return true
    const key = keyLabel(row.client_key_id)
    return `${row.model} ${row.request_id} ${row.status} ${key} ${row.error ?? ''} ${row.error_code ?? ''}`.includes(filter.value)
  }) ?? []
}
function duration(row: Row): string {
  if (row.finished_at_ms == null) return '—'
  return `${row.finished_at_ms - row.started_at_ms} ms`
}
function tokens(row: Row): string {
  if (!row.usage) return '—'
  return `${row.usage.input_tokens ?? '—'} / ${row.usage.output_tokens ?? '—'}`
}
function statusText(row: Row): string {
  const map: Record<string, string> = {
    succeeded: '成功', completed: '成功', running: '执行中', queued: '排队中',
    failed: '失败', cancelled: '已取消', rejected: '已拒绝', rejected_capacity: '容量已满', incomplete: '不完整',
  }
  return map[row.status] ?? row.status
}
function channelText(row: Row): string {
  if (row.source === 'unsigned') return '原生透传'
  if (row.source === 'host') return '仅观察'
  return row.excel ? 'Excel' : '原生'
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
let timer: ReturnType<typeof setInterval> | undefined
onMounted(() => {
  void refresh(true)
  void loadOptions()
  timer = setInterval(() => {
    if (tab.value === 'monitor')
      void refresh()
  }, 5000)
})
onUnmounted(() => {
  if (timer) clearInterval(timer)
})
</script>

<template>
  <main class="gateway">
    <nav aria-label="Excel 网关页面">
      <BaseButton :variant="tab === 'monitor' ? 'primary' : 'secondary'" @click="tab = 'monitor'">
        请求监控
      </BaseButton>
      <BaseButton :variant="tab === 'settings' ? 'primary' : 'secondary'" @click="tab = 'settings'; loadOptions(); refresh(true)">
        网关设置
      </BaseButton>
      <span v-if="snapshot" class="summary">
        Excel 执行 {{ snapshot.active }} · 排队 {{ snapshot.waiting }} · {{ snapshot.policy.enabled ? '通道开启' : '通道关闭' }}
      </span>
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

    <template v-if="tab === 'monitor'">
      <div class="cards">
        <div class="card">
          <div class="num">{{ stats.excel }}</div>
          <div class="label">Excel 成功</div>
        </div>
        <div class="card">
          <div class="num bad">{{ stats.excelFailed }}</div>
          <div class="label">Excel 失败/取消</div>
        </div>
        <div class="card">
          <div class="num">{{ stats.passthrough }}</div>
          <div class="label">未签名透传</div>
        </div>
        <div class="card">
          <div class="num">{{ stats.native }}</div>
          <div class="label">已签名走原生</div>
        </div>
        <div class="card">
          <div class="num">{{ stats.total }}</div>
          <div class="label">记录总数</div>
        </div>
      </div>
      <BaseCard>
        <div class="toolbar">
          <label for="filter">筛选 <input id="filter" v-model="filter" placeholder="Key、模型、状态或错误"></label>
          <small>最近 1000 条 · 进程重启后清空</small>
        </div>
        <div class="table-scroll">
          <table>
            <thead>
              <tr>
                <th>时间</th><th>Key</th><th>模型</th><th>通道</th><th>状态</th><th>耗时</th><th>Tokens 入/出</th><th>错误</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="row in rows()" :key="row.request_id + row.started_at_ms">
                <td>{{ new Date(row.started_at_ms).toLocaleTimeString() }}</td>
                <td>{{ keyLabel(row.client_key_id) }}</td>
                <td>
                  {{ row.model }}<small v-if="row.upstream_model && row.upstream_model !== row.model">→ {{ row.upstream_model }}</small>
                </td>
                <td>{{ channelText(row) }}</td>
                <td :class="{ bad: ['failed', 'rejected', 'rejected_capacity'].includes(row.status) }">
                  {{ statusText(row) }}
                </td>
                <td>{{ duration(row) }}</td>
                <td>{{ tokens(row) }}</td>
                <td class="error-cell" :title="row.error || row.error_code || ''">
                  {{ row.error || row.error_code || '—' }}
                </td>
              </tr>
              <tr v-if="rows().length === 0">
                <td colspan="8" class="empty">
                  暂无请求记录
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </BaseCard>
    </template>

    <template v-if="tab === 'settings' && policy">
      <BaseCard>
        <div class="row-line">
          <label class="switch-line">
            <input v-model="policy.enabled" type="checkbox"> Excel 通道开启
          </label>
          <small>关闭后所有请求走原生，即使模型带 -excel 后缀</small>
        </div>
        <div class="grid">
          <label>最大并发 <input v-model.number="policy.concurrency" type="number" min="1" max="1024" required></label>
          <label>超出并发 <select v-model="policy.overflow"><option value="queue">排队</option><option value="reject">拒绝</option></select></label>
          <label v-if="policy.overflow === 'queue'">队列上限 <input v-model.number="policy.queue_capacity" type="number" min="0" max="10000" required></label>
          <label v-if="policy.overflow === 'queue'">排队超时 ms <input v-model.number="policy.queue_timeout_ms" type="number" min="1" max="600000" required></label>
        </div>
      </BaseCard>

      <BaseCard>
        <div class="card-title">
          模型通道（按基础模型名）
          <small>跟随后缀 = 客户端用带 -excel 的名字走 Excel；Excel = 无后缀也强制走 Excel；原生 = 带 -excel 后缀也压回原生。对签名与未签名请求都生效。</small>
        </div>
        <div class="chips">
          <div v-for="model in knownModels" :key="model" class="chip model" :class="'state-' + modelState(model)">
            <span class="chip-name">{{ model }}</span>
            <span class="seg">
              <button type="button" class="seg-btn" :class="{ active: modelState(model) === 'suffix' }" @click="setModelState(model, 'suffix')">跟随后缀</button>
              <button type="button" class="seg-btn" :class="{ active: modelState(model) === 'excel' }" @click="setModelState(model, 'excel')">Excel</button>
              <button type="button" class="seg-btn" :class="{ active: modelState(model) === 'native' }" @click="setModelState(model, 'native')">原生</button>
            </span>
          </div>
          <span v-if="knownModels.length === 0" class="muted">暂无已知模型，发起请求后出现在这里</span>
        </div>
        <div class="grid">
          <label>添加模型 <input v-model="newModel" placeholder="模型名，如 gpt-5.6-sol"></label>
          <button type="button" class="add-btn" @click="if (newModel.trim()) { setModelState(newModel.trim(), 'excel'); newModel = '' }">
            添加并强制 Excel
          </button>
        </div>
      </BaseCard>

      <BaseCard>
        <div class="card-title">
          账户范围
          <small>哪些账户允许服务 Excel 请求；全部不勾选表示不限</small>
        </div>
        <div class="chips">
          <label v-for="account in accountList" :key="account.id" class="chip account" :class="{ off: !accountAllowed(account.id) }">
            <input type="checkbox" :checked="accountAllowed(account.id)" @change="setAccountAllowed(account.id, ($event.target as HTMLInputElement).checked)">
            <span class="chip-name">{{ account.id.slice(0, 20) }}…</span>
            <span v-if="!account.enabled" class="chip-state">已停用</span>
          </label>
          <span v-if="accountList.length === 0" class="muted">暂无账户</span>
        </div>
      </BaseCard>

      <BaseCard>
        <div class="card-title">
          插件设置（在 CPR 插件实例编辑器中修改）
          <small v-if="pluginInfo">
            excelMode={{ pluginInfo.excelMode }} · 模型后缀={{ pluginInfo.excelModelSuffix }} · 总开关={{ pluginInfo.excelEnabled ? '开' : '关' }} · 作用域={{ pluginInfo.isolationScope }}
          </small>
        </div>
      </BaseCard>

      <BaseCard>
        <div class="card-title">
          按 Key 授权（学到的使用情况）
          <small>哪些 Key 能使用 Excel 通道由 CPR 插件实例的「绑定」决定：绑定 clientKeyIds 后命中的请求可走 Excel，未绑定自动原生透传。下表是最近请求里实际出现过的 Key。</small>
        </div>
        <div class="chips">
          <div v-for="key in learnedKeys" :key="key.id" class="chip">
            <span class="chip-name">{{ key.name }}</span>
            <span class="chip-state">Excel {{ key.excel }} 次 · 原生 {{ key.native }} 次</span>
          </div>
          <span v-if="learnedKeys.length === 0" class="muted">暂无数据</span>
        </div>
      </BaseCard>

      <div class="toolbar">
        <BaseButton type="button" :disabled="saving" @click="save()">
          {{ saving ? '保存中' : '保存配置' }}
        </BaseButton>
        <small>有运行中的 Excel 请求时保存会被拒绝，稍后重试</small>
      </div>
    </template>
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
.toolbar,
.grid,
.row-line {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}
.summary {
  margin-left: auto;
  color: var(--cp-color-text-secondary);
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
  width: 110px;
}
label {
  display: flex;
  align-items: center;
  gap: 8px;
}
.cards {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
  gap: 12px;
}
.card {
  border: 1px solid var(--cp-color-border, #8884);
  border-radius: 8px;
  padding: 12px 16px;
}
.card .num {
  font-size: 22px;
  font-weight: 600;
}
.card .num.bad {
  color: var(--cp-color-error, #b42318);
}
.card .label {
  color: var(--cp-color-text-secondary);
  font-size: 12px;
  margin-top: 2px;
}
.card-title {
  font-weight: 600;
  margin-bottom: 10px;
}
.chips {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-bottom: 12px;
}
.chip {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  border: 1px solid var(--cp-color-border, #8886);
  border-radius: 16px;
  padding: 4px 12px;
  font-size: 13px;
}
.chip.on {
  border-color: var(--cp-color-success, #2e7d32);
}
.chip.off {
  opacity: 0.65;
}
.chip-state {
  color: var(--cp-color-text-secondary);
  font-size: 12px;
}
.seg {
  display: inline-flex;
  gap: 0;
}
.seg-btn {
  font: inherit;
  font-size: 12px;
  border: 1px solid var(--cp-color-border, #8886);
  background: var(--cp-color-bg-container, transparent);
  color: inherit;
  cursor: pointer;
  padding: 2px 8px;
}
.seg-btn + .seg-btn {
  border-left: none;
}
.seg-btn.active {
  background: var(--cp-color-primary, #06f);
  color: #fff;
  border-color: var(--cp-color-primary, #06f);
}
.chip.model.state-excel {
  border-color: var(--cp-color-success, #2e7d32);
}
.chip.model.state-native {
  border-color: var(--cp-color-error, #b42318);
}
.chip-btn {
  font: inherit;
  font-size: 12px;
  border: none;
  background: var(--cp-color-bg-container, transparent);
  color: var(--cp-color-primary, #06f);
  cursor: pointer;
  padding: 0;
}
.add-btn {
  font: inherit;
  border: 1px solid var(--cp-color-border, #8886);
  border-radius: 6px;
  background: var(--cp-color-bg-container, transparent);
  color: inherit;
  cursor: pointer;
  padding: 7px 12px;
}
.muted {
  color: var(--cp-color-text-secondary);
}
.table-scroll {
  overflow: auto;
}
table {
  width: 100%;
  border-collapse: collapse;
  text-align: left;
  white-space: nowrap;
  margin-top: 12px;
}
td,
th {
  padding: 9px 10px;
  border-bottom: 1px solid var(--cp-color-border, #8883);
}
th {
  font-weight: 500;
  color: var(--cp-color-text-secondary);
}
td small {
  margin-left: 6px;
}
.empty {
  text-align: center;
  padding: 40px;
}
.error {
  color: var(--cp-color-error, #b42318);
}
.error-cell {
  max-width: 300px;
  overflow: hidden;
  text-overflow: ellipsis;
  color: var(--cp-color-error, #b42318);
}
.bad {
  color: var(--cp-color-error, #b42318);
}
@media (max-width: 700px) {
  .summary {
    margin-left: 0;
  }
  label {
    flex-wrap: wrap;
  }
}
</style>
