<script setup lang="ts">
import type { Policy } from './gateway'
import { BaseButton, BaseCard } from '@codex-proxy/ui'

import { computed, onMounted, onUnmounted, ref } from 'vue'
import { setChannel } from './gateway'
import RoutingSettings from './RoutingSettings.vue'

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
const refreshError = ref('')
const success = ref('')
const saving = ref(false)
const tab = ref<'monitor' | 'settings'>('monitor')
const filter = ref('')
const options = ref<{ accounts: { account_id: string, enabled: boolean }[], keys: { id: string, name: string, enabled: boolean }[] }>({ accounts: [], keys: [] })
const pluginInfo = ref<{ excelModelSuffix?: string, isolationScope: string, routingMode?: 'host_binding' } | null>(null)
const optionsError = ref('')
// Excel 通道预制模型（原项目固定四款），无需手动添加
const PRESET_MODELS = ['gpt-5.6-sol', 'gpt-5.6-terra', 'gpt-5.6-luna', 'gpt-6-astra'] as const
const channelFilter = ref<'all' | 'excel' | 'native' | 'unsigned'>('all')

const keyNames = computed(() => {
  const map = new Map<string, string>()
  for (const key of options.value.keys)
    map.set(key.id, key.name)
  return map
})
function keyLabel(id: string | null): string {
  if (!id || id === 'unsigned')
    return '未签名'
  return keyNames.value.get(id) ?? `${id.slice(0, 14)}…`
}
function baseName(model: string): string {
  const suffix = pluginInfo.value?.excelModelSuffix ?? '-excel'
  return model.endsWith(suffix) && model.length > suffix.length ? model.slice(0, -suffix.length) : model
}
const knownModels = computed(() => {
  const set = new Set<string>(PRESET_MODELS)
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
  for (const id of policy.value?.accounts.allow ?? []) {
    if (!map.has(id))
      map.set(id, true)
  }
  for (const id of policy.value?.accounts.deny ?? []) {
    if (!map.has(id))
      map.set(id, true)
  }
  return [...map.entries()].map(([id, enabled]) => ({ id, enabled }))
})
function accountAllowed(id: string): boolean {
  const p = policy.value
  if (!p)
    return true
  if (p.accounts.deny.includes(id))
    return false
  if (p.accounts.allow.length > 0 && !p.accounts.allow.includes(id))
    return false
  return true
}
function setAccountAllowed(id: string, allowed: boolean) {
  const p = policy.value
  if (!p)
    return
  p.accounts.deny = p.accounts.deny.filter(a => a !== id)
  if (allowed) {
    if (p.accounts.allow.length > 0)
      p.accounts.allow = [...new Set([...p.accounts.allow, id])]
  }
  else {
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
    refreshError.value = ''
    return true
  }
  catch (cause) {
    refreshError.value = cause instanceof Error ? cause.message : '读取失败'
    return false
  }
}
async function loadOptions() {
  try {
    options.value = await api('api/options')
    optionsError.value = ''
  }
  catch { optionsError.value = '无法读取 Client Key 列表，请刷新重试。' }
  try {
    pluginInfo.value = await api('api/plugin-info')
  }
  catch { optionsError.value = '读取插件连接信息失败，请刷新重试。' }
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
      else if (['failed', 'cancelled', 'rejected', 'rejected_capacity', 'incomplete'].includes(row.status))
        excelFailed += 1
    }
    else if (row.source === 'unsigned') {
      passthrough += 1
    }
    else if (row.status === 'succeeded' || row.status === 'completed') {
      native += 1
    }
  }
  return { excel, excelFailed, passthrough, native, total: rows.length }
})
function rowChannel(row: Row): 'excel' | 'native' | 'unsigned' {
  if (row.source === 'unsigned')
    return 'unsigned'
  return row.excel ? 'excel' : 'native'
}
function rows() {
  return snapshot.value?.records.filter((row) => {
    if (channelFilter.value !== 'all' && rowChannel(row) !== channelFilter.value)
      return false
    if (!filter.value)
      return true
    const key = keyLabel(row.client_key_id)
    return `${row.model} ${row.request_id} ${row.status} ${key} ${row.error ?? ''} ${row.error_code ?? ''}`.includes(filter.value)
  }) ?? []
}
function duration(row: Row): string {
  if (row.finished_at_ms == null)
    return '—'
  return `${row.finished_at_ms - row.started_at_ms} ms`
}
function tokens(row: Row): string {
  if (!row.usage)
    return '—'
  return `${row.usage.input_tokens ?? '—'} / ${row.usage.output_tokens ?? '—'}`
}
function statusText(row: Row): string {
  const map: Record<string, string> = {
    succeeded: '成功',
    completed: '成功',
    running: '执行中',
    queued: '排队中',
    failed: '失败',
    cancelled: '已取消',
    rejected: '已拒绝',
    rejected_capacity: '容量已满',
    incomplete: '不完整',
  }
  return map[row.status] ?? row.status
}
function channelText(row: Row): string {
  const c = rowChannel(row)
  return c === 'excel' ? 'Excel' : c === 'unsigned' ? '未签名透传' : '原生'
}
async function save() {
  saving.value = true
  success.value = ''
  error.value = ''
  try {
    const saved = await api('api/policy', { policy: policy.value, expected_version: version.value })
    version.value = saved.version
    const refreshed = await refresh(true)
    success.value = refreshed ? '配置已保存' : '配置已保存，但重新读取失败；请刷新确认。'
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
  if (timer)
    clearInterval(timer)
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
    <p v-if="error || refreshError" class="error" role="alert">
      {{ error || refreshError }}
    </p>
    <p v-if="success" role="status">
      {{ success }}
    </p>

    <template v-if="tab === 'monitor'">
      <div class="cards">
        <div class="card">
          <div class="num">
            {{ stats.excel }}
          </div>
          <div class="label">
            Excel 成功
          </div>
        </div>
        <div class="card">
          <div class="num bad">
            {{ stats.excelFailed }}
          </div>
          <div class="label">
            Excel 失败/取消
          </div>
        </div>
        <div class="card">
          <div class="num">
            {{ stats.passthrough }}
          </div>
          <div class="label">
            未签名透传
          </div>
        </div>
        <div class="card">
          <div class="num">
            {{ stats.native }}
          </div>
          <div class="label">
            已签名走原生
          </div>
        </div>
        <div class="card">
          <div class="num">
            {{ stats.total }}
          </div>
          <div class="label">
            记录总数
          </div>
        </div>
      </div>
      <BaseCard>
        <div class="toolbar">
          <label for="filter">筛选 <input id="filter" v-model="filter" placeholder="Key、模型、状态或错误"></label>
          <span class="seg">
            <button type="button" class="seg-btn" :class="{ active: channelFilter === 'all' }" @click="channelFilter = 'all'">全部</button>
            <button type="button" class="seg-btn" :class="{ active: channelFilter === 'excel' }" @click="channelFilter = 'excel'">Excel</button>
            <button type="button" class="seg-btn" :class="{ active: channelFilter === 'native' }" @click="channelFilter = 'native'">原生</button>
            <button type="button" class="seg-btn" :class="{ active: channelFilter === 'unsigned' }" @click="channelFilter = 'unsigned'">未签名</button>
          </span>
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

    <form v-if="tab === 'settings' && policy" class="gateway-form" @submit.prevent="save">
      <BaseCard>
        <div class="row-line">
          <div>
            <h2 class="section-title">
              通道与路由
            </h2><small>为 CPR 绑定范围内的请求设置默认通道；不修改 Key 授权。</small>
          </div>
          <label for="gateway-enabled" class="switch-line"><input id="gateway-enabled" v-model="policy.enabled" type="checkbox"> Excel 通道开启</label>
        </div>
        <p v-if="!policy.enabled" class="muted">
          Excel 通道已关闭，桥接请求走原生；已配置的模型通道会保留。
        </p>
        <RoutingSettings :policy="policy" :models="knownModels" :suffix="pluginInfo?.excelModelSuffix ?? '-excel'" @change="(model, channel) => policy && setChannel(policy, model, channel)" />
        <details class="gateway-details">
          <summary>Client Key 与模型的适用范围</summary>
          <p class="muted">
            在 CPR「插件管理 → 当前配置 → 绑定」中选择 Client Key、账号组与模型范围。宿主先匹配范围，命中后才调用插件；未命中的请求不应用这里的通道规则。
          </p>
          <p class="muted">
            当前绑定范围共用上述模型默认通道，不等于每个 Key 都有一套独立规则。这里不会保存不生效的 Key 覆盖。
          </p>
          <p v-if="Object.keys(policy.key_rules).length" role="status">
            检测到历史 Key 覆盖配置：数据保留，但当前不会参与路由。
          </p>
          <p v-if="optionsError" role="alert">
            {{ optionsError }}
          </p>
        </details>
      </BaseCard>
      <BaseCard>
        <h2 class="section-title">
          并发与排队
        </h2>
        <div class="grid">
          <label for="gateway-concurrency">Excel 最大并发 <input id="gateway-concurrency" v-model.number="policy.concurrency" type="number" min="1" max="1024" required></label>
          <label for="gateway-overflow">超出并发 <select id="gateway-overflow" v-model="policy.overflow"><option value="queue">排队</option><option value="reject">拒绝</option></select></label>
          <label v-if="policy.overflow === 'queue'" for="gateway-queue-capacity">队列上限 <input id="gateway-queue-capacity" v-model.number="policy.queue_capacity" type="number" min="0" max="10000" required></label>
          <label v-if="policy.overflow === 'queue'" for="gateway-queue-timeout">排队超时（毫秒） <input id="gateway-queue-timeout" v-model.number="policy.queue_timeout_ms" type="number" min="1" max="600000" required></label>
        </div>
        <details class="gateway-details">
          <summary>Excel 可用账户范围</summary>
          <p class="muted">
            允许哪些账户处理 Excel 请求；排除账户不会影响该账户的原生通道，也不会修改 CPR 的账户授权。
          </p>
          <div class="chips">
            <label v-for="account in accountList" :key="account.id" :for="`account-${account.id}`" class="chip account" :class="{ off: !accountAllowed(account.id) }">
              <input :id="`account-${account.id}`" type="checkbox" :checked="accountAllowed(account.id)" @change="setAccountAllowed(account.id, ($event.target as HTMLInputElement).checked)">
              <span class="chip-name" :title="account.id">{{ account.id.slice(0, 20) }}…</span><span v-if="!account.enabled" class="chip-state">已停用</span>
            </label>
          </div>
        </details>
      </BaseCard>
      <BaseCard>
        <details class="gateway-details">
          <summary>连接与诊断</summary>
          <div class="diagnostic-grid">
            <span>网关连接</span><strong>{{ snapshot ? '正常' : '不可用' }}</strong><span>Key 范围管理</span><strong>CPR 原生插件绑定</strong><span>实例隔离范围</span><code>{{ pluginInfo?.isolationScope ?? '读取中' }}</code>
          </div>
          <p class="muted">
            连接地址、签名密钥路径属于部署配置，不需要日常调整。这里不读取密钥内容；安装、升级、权限授权和故障修复仍在 CPR 插件管理中处理。
          </p>
        </details>
      </BaseCard>
      <div class="toolbar">
        <BaseButton type="submit" :disabled="saving">
          {{ saving ? '保存中' : '保存网关设置' }}
        </BaseButton>
        <small>运行或排队请求尚未结束时不会修改配置，请待请求结束后保存。</small>
      </div>
    </form>
  </main>
</template>

<style>
.gateway-form {
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.section-title {
  margin: 0 0 10px;
  font-size: 16px;
}
.gateway-details {
  margin-top: 12px;
}
.gateway-details summary {
  cursor: pointer;
  font-weight: 600;
}
.diagnostic-grid {
  display: grid;
  grid-template-columns: max-content 1fr;
  gap: 12px 24px;
  margin: 18px 0;
  font-size: 13px;
}

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
