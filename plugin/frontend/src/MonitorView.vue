<script setup lang="ts">
import type { ClientKey, RequestRow, Snapshot } from './gateway'
import { ChevronLeft, ChevronRight, Search } from '@lucide/vue'
import { computed, ref, watch } from 'vue'
import { formatDuration, formatTokens, rowChannel, statusLabel, statusTone, tokensPerSecond } from './gateway'

const props = defineProps<{
  snapshot: Snapshot
  keys: ClientKey[]
  scope: string
}>()
const autoRefresh = defineModel<boolean>('autoRefresh', { required: true })
const search = ref('')
const channel = ref('all')
const status = ref('all')
const page = ref(1)
const expanded = ref<string | null>(null)
const pageSize = 15
const channels = [
  { value: 'all', label: '全部通道' },
  { value: 'excel', label: 'Excel' },
  { value: 'native', label: '原生' },
  { value: 'unsigned', label: '未签名' },
]
const keyMap = computed(
  () => new Map(props.keys.map(key => [key.id, key.name])),
)
function keyLabel(id: string | null) {
  if (!id || id === 'unsigned')
    return '未识别 Key'
  if (id === props.scope && !keyMap.value.has(id))
    return '待归并 Key'
  return keyMap.value.get(id) ?? id
}
const filtered = computed(() =>
  props.snapshot.records.filter((row) => {
    const needle = search.value.trim().toLowerCase()
    return (
      (channel.value === 'all' || rowChannel(row) === channel.value)
      && (status.value === 'all'
        || (status.value === 'error'
          ? statusTone(row.status) === 'danger'
          : status.value === 'completed'
            ? statusTone(row.status) === 'success'
            : row.status === status.value))
          && (!needle
            || [
              row.request_id,
              row.client_key_id,
              keyLabel(row.client_key_id),
              row.account_id,
              row.model,
              row.upstream_model,
              row.status,
              statusLabel(row.status),
              row.error_code,
              row.error,
            ]
              .join(' ')
              .toLowerCase()
              .includes(needle))
    )
  }),
)
const pageCount = computed(() =>
  Math.max(1, Math.ceil(filtered.value.length / pageSize)),
)
const visible = computed(() =>
  filtered.value.slice((page.value - 1) * pageSize, page.value * pageSize),
)
watch([search, channel, status], () => {
  page.value = 1
  expanded.value = null
})
watch(pageCount, (count) => {
  if (page.value > count)
    page.value = count
})
const stats = computed(() => {
  const rows = props.snapshot.records
  return {
    total: rows.length,
    success: rows.filter(row => statusTone(row.status) === 'success').length,
    failed: rows.filter(row => statusTone(row.status) === 'danger').length,
    cancelled: rows.filter(row => row.status === 'cancelled').length,
    excel: rows.filter(row => rowChannel(row) === 'excel').length,
  }
})
function time(ms: number) {
  return new Date(ms).toLocaleTimeString('zh-CN', { hour12: false })
}
function duration(row: RequestRow) {
  return formatDuration(
    row.usage?.timings?.latency_ms
    ?? (row.finished_at_ms === null ? null : row.finished_at_ms - row.started_at_ms),
  )
}
function speed(row: RequestRow) {
  const value = tokensPerSecond(row.usage)
  return value === null ? '—' : value.toFixed(1)
}
function reset() {
  search.value = ''
  channel.value = 'all'
  status.value = 'all'
}
</script>

<template>
  <section class="gw-monitor" aria-label="请求监控">
    <div class="gw-status-strip" aria-label="最近请求统计">
      <span>
        执行
        <b>{{ snapshot.active }} / {{ snapshot.policy.concurrency }}</b>
      </span>
      <span>
        排队 <b>{{ snapshot.waiting }}</b>
      </span>
      <span class="gw-stat-separator">
        最近 <b>{{ stats.total }}</b> 条
      </span>
      <span>
        完成 <b>{{ stats.success }}</b>
      </span>
      <span>
        异常
        <b :class="{ 'gw-danger-text': stats.failed > 0 }">
          {{ stats.failed }}
        </b>
      </span>
      <span>
        取消 <b>{{ stats.cancelled }}</b>
      </span>
    </div>
    <div class="gw-toolbar">
      <label class="gw-search" for="gw-request-search">
        <Search :size="15" aria-hidden="true" /><input
          id="gw-request-search"
          v-model="search"
          type="search"
          aria-label="搜索请求"
          placeholder="搜索模型、Key 或请求 ID"
        >
      </label>
      <select v-model="channel" aria-label="筛选请求通道">
        <option v-for="item in channels" :key="item.value" :value="item.value">
          {{ item.label }}
        </option>
      </select>
      <select v-model="status" aria-label="筛选请求状态">
        <option value="all">
          所有状态
        </option>
        <option value="completed">
          已完成
        </option>
        <option value="error">
          异常
        </option>
        <option value="running">
          执行中
        </option>
        <option value="queued">
          排队中
        </option>
        <option value="cancelled">
          已取消
        </option>
      </select>
      <label class="gw-check gw-toolbar-end" for="gw-auto-refresh">
        <input
          id="gw-auto-refresh"
          v-model="autoRefresh"
          type="checkbox"
        >自动刷新
      </label>
    </div>
    <div class="gw-table-scroll">
      <table class="gw-table gw-requests">
        <thead>
          <tr>
            <th>时间</th>
            <th>模型</th>
            <th>Client Key</th>
            <th>通道</th>
            <th>状态</th>
            <th
              class="gw-number"
              title="首字为宿主记录的首个输出 Token 延迟；完成为请求总耗时。缺失计时显示 —。"
            >
              首字 / 完成
            </th>
            <th class="gw-number">
              Tokens 入 / 出
            </th>
            <th
              class="gw-number"
              title="输出 Tokens ÷（完成耗时 − 首字耗时），使用宿主同一条计时；数据不足时显示 —。"
            >
              tok/s
            </th>
            <th><span class="gw-sr-only">操作</span></th>
          </tr>
        </thead>
        <tbody>
          <template v-for="row in visible" :key="row.request_id">
            <tr :class="{ 'is-expanded': expanded === row.request_id }">
              <td class="gw-mono">
                {{ time(row.started_at_ms) }}
              </td>
              <td class="gw-model">
                {{ row.model }}
              </td>
              <td>
                <span
                  class="gw-truncate"
                  :title="keyLabel(row.client_key_id)"
                >
                  {{ keyLabel(row.client_key_id) }}
                </span>
              </td>
              <td>
                {{
                  rowChannel(row) === "excel"
                    ? "Excel"
                    : rowChannel(row) === "unsigned"
                      ? "未签名"
                      : "原生"
                }}
              </td>
              <td>
                <span class="gw-status" :class="`is-${statusTone(row.status)}`">
                  <span class="gw-dot" />{{ statusLabel(row.status) }}
                </span>
              </td>
              <td class="gw-number gw-mono">
                {{ formatDuration(row.usage?.timings?.first_token_ms ?? null) }}
                <span class="gw-subtle">/</span>
                {{ duration(row) }}
              </td>
              <td class="gw-number gw-mono">
                {{ formatTokens(row.usage?.input_tokens) }}
                <span class="gw-subtle">/</span>
                {{ formatTokens(row.usage?.output_tokens) }}
              </td>
              <td class="gw-number gw-mono">
                {{ speed(row) }}
              </td>
              <td>
                <button
                  class="gw-text-button"
                  type="button"
                  :aria-label="`查看请求 ${row.request_id}`"
                  :aria-expanded="expanded === row.request_id"
                  @click="
                    expanded
                      = expanded === row.request_id ? null : row.request_id
                  "
                >
                  {{ expanded === row.request_id ? "收起" : "详情" }}
                </button>
              </td>
            </tr>
            <tr v-if="expanded === row.request_id" class="gw-detail-row">
              <td colspan="9">
                <dl class="gw-request-detail">
                  <div>
                    <dt>请求 ID</dt>
                    <dd>{{ row.request_id }}</dd>
                  </div>
                  <div>
                    <dt>Client Key ID</dt>
                    <dd>{{ row.client_key_id || "未提供" }}</dd>
                  </div>
                  <div>
                    <dt>账户 ID</dt>
                    <dd>{{ row.account_id || "未提供" }}</dd>
                  </div>
                  <div>
                    <dt>排队耗时</dt>
                    <dd>{{ formatDuration(row.queue_ms) }}</dd>
                  </div>
                  <div>
                    <dt>上游模型</dt>
                    <dd>{{ row.upstream_model || "未返回" }}</dd>
                  </div>
                  <div>
                    <dt>来源 / 错误码</dt>
                    <dd>{{ row.source }} / {{ row.error_code || "无" }}</dd>
                  </div>
                </dl>
                <p v-if="row.error" class="gw-error-detail">
                  {{ row.error }}
                </p>
              </td>
            </tr>
          </template>
          <tr v-if="!visible.length">
            <td colspan="9" class="gw-empty">
              <p>
                {{ snapshot.records.length ? "没有匹配的请求" : "暂无请求记录" }}
              </p>
              <button
                v-if="snapshot.records.length"
                class="gw-text-button"
                type="button"
                @click="reset"
              >
                清除筛选
              </button>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
    <div class="gw-pagination">
      <span class="gw-subtle">保留最近 1000 条 · 重启后清空</span>
      <div>
        <span>
          {{ filtered.length ? (page - 1) * pageSize + 1 : 0 }}–{{ Math.min(page * pageSize, filtered.length) }}
          / {{ filtered.length }} 条
        </span>
        <button
          class="gw-icon-button"
          type="button"
          aria-label="上一页"
          :disabled="page === 1"
          @click="page--"
        >
          <ChevronLeft :size="16" />
        </button><span>{{ page }} / {{ pageCount }}</span><button
          class="gw-icon-button"
          type="button"
          aria-label="下一页"
          :disabled="page === pageCount"
          @click="page++"
        >
          <ChevronRight :size="16" />
        </button>
      </div>
    </div>
  </section>
</template>
