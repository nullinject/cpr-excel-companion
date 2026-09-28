<script setup lang="ts">
import type { ClientKey, RequestRow, Snapshot } from './gateway'
import { ChevronDown, ChevronLeft, ChevronRight, Search } from '@lucide/vue'
import { computed, ref, watch } from 'vue'
import { formatDuration, rowChannel, statusLabel, statusTone } from './gateway'

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
    row.finished_at_ms === null ? null : row.finished_at_ms - row.started_at_ms,
  )
}
function token(value: number | undefined) {
  return value === undefined ? '—' : value.toLocaleString('zh-CN')
}
function reset() {
  search.value = ''
  channel.value = 'all'
  status.value = 'all'
}
</script>

<template>
  <section class="gw-monitor" aria-label="请求监控">
    <div class="gw-page-heading">
      <div>
        <h2>请求概览</h2>
        <p>最近记录的运行状态与通道分布</p>
      </div>
      <label class="gw-check" for="gw-auto-refresh">
        <input id="gw-auto-refresh" v-model="autoRefresh" type="checkbox">每
        5 秒自动刷新
      </label>
    </div>
    <div class="gw-metrics">
      <article class="gw-metric">
        <span class="gw-eyebrow">Excel 实时负载</span>
        <div class="gw-metric-number">
          {{ snapshot.active }}<span>/ {{ snapshot.policy.concurrency }}</span>
        </div>
        <div class="gw-metric-meta">
          <span>正在执行</span><span>
            排队 <b>{{ snapshot.waiting }}</b>
          </span>
        </div>
      </article>
      <article class="gw-metric">
        <span class="gw-eyebrow">最近请求</span>
        <div class="gw-metric-number">
          {{ stats.total }}<span>条</span>
        </div>
        <div class="gw-metric-meta">
          <span>Excel 通道 {{ stats.excel }} 条</span>
        </div>
      </article>
      <article class="gw-metric">
        <span class="gw-eyebrow">已完成</span>
        <div class="gw-metric-number gw-success-text">
          {{ stats.success }}<span>条</span>
        </div>
        <div class="gw-metric-meta">
          <span>所有通道 · 仅成功终态</span>
        </div>
      </article>
      <article class="gw-metric">
        <span class="gw-eyebrow">需关注</span>
        <div
          class="gw-metric-number"
          :class="{ 'gw-danger-text': stats.failed > 0 }"
        >
          {{ stats.failed }}<span>条</span>
        </div>
        <div class="gw-metric-meta">
          <span>失败 / 拒绝 / 不完整</span><span>取消 {{ stats.cancelled }}</span>
        </div>
      </article>
    </div>
    <section class="gw-card" aria-labelledby="requests-title">
      <div class="gw-card-heading">
        <div>
          <h3 id="requests-title">
            请求记录 <span class="gw-count">{{ filtered.length }}</span>
          </h3>
          <p>最近 1000 条 · 进程重启后清空 · 点击详情查看完整标识</p>
        </div>
      </div>
      <div class="gw-toolbar">
        <label class="gw-search" for="gw-request-search">
          <Search :size="16" aria-hidden="true" /><input
            id="gw-request-search"
            v-model="search"
            type="search"
            aria-label="搜索请求"
            placeholder="搜索 Key、模型、请求 ID 或错误"
          >
        </label><select v-model="status" aria-label="筛选请求状态">
          <option value="all">
            所有状态
          </option>
          <option value="completed">
            已完成
          </option>
          <option value="error">
            需关注
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
      </div>
      <div class="gw-filter-row">
        <div class="gw-segmented" aria-label="筛选请求通道">
          <button
            v-for="item in channels"
            :key="item.value"
            type="button"
            :aria-pressed="channel === item.value"
            @click="channel = item.value"
          >
            {{ item.label }}
          </button>
        </div>
        <span class="gw-subtle">
          {{ filtered.length ? (page - 1) * pageSize + 1 : 0 }}–{{ Math.min(page * pageSize, filtered.length) }}
          / {{ filtered.length }} 条
        </span>
      </div>
      <div class="gw-table-scroll">
        <table class="gw-table gw-requests">
          <thead>
            <tr>
              <th>时间 / Key</th>
              <th>模型</th>
              <th>通道</th>
              <th>状态</th>
              <th class="gw-number">
                耗时
              </th>
              <th class="gw-number">
                Tokens 入 / 出
              </th>
              <th><span class="gw-sr-only">请求详情</span></th>
            </tr>
          </thead>
          <tbody>
            <template v-for="row in visible" :key="row.request_id">
              <tr :class="{ 'is-expanded': expanded === row.request_id }">
                <td>
                  <span class="gw-mono">{{ time(row.started_at_ms) }}</span><span
                    class="gw-cell-secondary gw-truncate"
                    :title="keyLabel(row.client_key_id)"
                  >
                    {{ keyLabel(row.client_key_id) }}
                  </span>
                </td>
                <td>
                  <span class="gw-model">{{ row.model }}</span><span
                    v-if="
                      row.upstream_model && row.upstream_model !== row.model
                    "
                    class="gw-cell-secondary"
                  >
                    → {{ row.upstream_model }}
                  </span>
                </td>
                <td>
                  <span
                    class="gw-badge"
                    :class="
                      rowChannel(row) === 'excel' ? 'is-primary' : 'is-neutral'
                    "
                  >
                    {{
                      rowChannel(row) === "excel"
                        ? "Excel"
                        : rowChannel(row) === "unsigned"
                          ? "未签名"
                          : "原生"
                    }}
                  </span>
                </td>
                <td>
                  <span
                    class="gw-status"
                    :class="`is-${statusTone(row.status)}`"
                  >
                    <span class="gw-dot" />{{ statusLabel(row.status) }}
                  </span>
                </td>
                <td class="gw-number gw-mono">
                  {{ duration(row) }}
                </td>
                <td class="gw-number gw-mono">
                  {{ token(row.usage?.input_tokens) }}
                  <span class="gw-subtle">/</span>
                  {{ token(row.usage?.output_tokens) }}
                </td>
                <td>
                  <button
                    class="gw-icon-button"
                    type="button"
                    :aria-label="`查看请求 ${row.request_id}`"
                    :aria-expanded="expanded === row.request_id"
                    @click="
                      expanded
                        = expanded === row.request_id ? null : row.request_id
                    "
                  >
                    <ChevronDown
                      :size="16"
                      :class="{ 'gw-rotated': expanded === row.request_id }"
                    />
                  </button>
                </td>
              </tr>
              <tr v-if="expanded === row.request_id" class="gw-detail-row">
                <td colspan="7">
                  <div class="gw-request-detail">
                    <dl>
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
                        <dt>请求模型 → 上游模型</dt>
                        <dd>
                          {{ row.model }} → {{ row.upstream_model || "未返回" }}
                        </dd>
                      </div>
                      <div>
                        <dt>来源 / 错误码</dt>
                        <dd>{{ row.source }} / {{ row.error_code || "无" }}</dd>
                      </div>
                    </dl>
                    <p v-if="row.error" class="gw-error-detail">
                      {{ row.error }}
                    </p>
                    <p v-else class="gw-subtle">
                      该记录没有错误说明。
                    </p>
                  </div>
                </td>
              </tr>
            </template>
          </tbody>
        </table>
      </div>
      <div v-if="!visible.length" class="gw-empty">
        <Search :size="26" />
        <h3>
          {{ snapshot.records.length ? "没有匹配的请求" : "等待第一条请求" }}
        </h3>
        <p>
          {{
            snapshot.records.length
              ? "试试其他模型、Key 或状态，搜索支持完整 ID。"
              : "经过网关的请求会出现在这里，自动刷新不会打断设置编辑。"
          }}
        </p>
        <button
          v-if="snapshot.records.length"
          class="gw-button"
          type="button"
          @click="reset"
        >
          清除筛选
        </button>
      </div>
      <div v-if="filtered.length" class="gw-pagination">
        <span class="gw-subtle">每页 {{ pageSize }} 条</span>
        <div>
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
  </section>
</template>
