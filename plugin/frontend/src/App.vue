<script setup lang="ts">
import { Activity, RefreshCw } from '@lucide/vue'
import { computed, ref } from 'vue'
import MonitorView from './MonitorView.vue'
import SettingsView from './SettingsView.vue'
import { useGateway } from './useGateway'

const {
  snapshot,
  draft,
  keys,
  accounts,
  info,
  loading,
  refreshing,
  saving,
  catalogLoading,
  autoRefresh,
  refreshError,
  actionError,
  catalogErrors,
  infoError,
  notice,
  lastUpdated,
  dirty,
  conflicted,
  busy,
  refresh,
  loadCatalogs,
  loadInfo,
  discard,
  save,
} = useGateway()
const tab = ref<'monitor' | 'settings'>('monitor')
const syncLabel = computed(() =>
  refreshError.value
    ? '刷新失败 · 显示缓存'
    : loading.value
      ? '连接中'
      : autoRefresh.value
        ? '已同步'
        : '自动刷新已暂停',
)
const updatedLabel = computed(() =>
  lastUpdated.value
    ? new Date(lastUpdated.value).toLocaleTimeString('zh-CN', { hour12: false })
    : '尚未同步',
)
const models = computed(() => {
  const suffix = info.value?.excelModelSuffix ?? '-excel'
  const names = new Set([
    'gpt-5.6-sol',
    'gpt-5.6-terra',
    'gpt-5.6-luna',
    'gpt-6-astra',
    ...Object.keys(draft.value?.model_channels ?? {}),
  ])
  for (const row of snapshot.value?.records ?? []) {
    const name
      = suffix && row.model.endsWith(suffix)
        ? row.model.slice(0, -suffix.length)
        : row.model
    if (name)
      names.add(name)
  }
  return [...names].sort()
})
async function reload() {
  await Promise.all([refresh(), loadCatalogs(), loadInfo()])
}
</script>

<template>
  <main class="gw-console">
    <header class="gw-header">
      <h1>Excel 网关</h1>
      <div class="gw-header-actions">
        <div class="gw-sync" role="status">
          <span
            class="gw-dot"
            :class="{ 'is-warning': refreshError, 'is-muted': !autoRefresh }"
          />{{ syncLabel }}<small>{{ updatedLabel }}</small>
        </div>
        <button
          class="gw-button"
          type="button"
          :disabled="refreshing || saving"
          @click="reload"
        >
          <RefreshCw :size="15" :class="{ 'gw-spinning': refreshing }" />{{ refreshing ? "刷新中" : "刷新" }}
        </button>
      </div>
    </header>
    <nav class="gw-tabs" aria-label="Excel 网关页面">
      <button
        type="button"
        :aria-pressed="tab === 'monitor'"
        :class="{ 'is-current': tab === 'monitor' }"
        @click="tab = 'monitor'"
      >
        请求监控
      </button><button
        type="button"
        :aria-pressed="tab === 'settings'"
        :class="{ 'is-current': tab === 'settings' }"
        @click="tab = 'settings'"
      >
        网关设置<span
          v-if="dirty"
          class="gw-unsaved-dot"
          aria-hidden="true"
          title="有未保存改动"
        />
      </button><span v-if="snapshot" class="gw-tabs-meta">
        <span
          class="gw-dot"
          :class="{ 'is-muted': !snapshot.policy.enabled }"
        />Excel 通道{{ snapshot.policy.enabled ? "已开启" : "已关闭" }}
      </span>
    </nav>
    <div v-if="refreshError" class="gw-alert is-warning" role="alert">
      <strong>状态暂未更新</strong><span>
        {{ refreshError }}
        {{
          snapshot
            ? "保留上次数据；草稿不会被刷新覆盖。"
            : "请确认桥接服务在线后重试。"
        }}
      </span>
    </div>
    <div v-if="catalogErrors.length" class="gw-alert is-warning" role="alert">
      <strong>部分目录读取失败</strong><span>{{ catalogErrors.join("；") }}</span><button
        class="gw-text-button"
        type="button"
        :disabled="catalogLoading"
        @click="loadCatalogs"
      >
        重新读取
      </button>
    </div>
    <div v-if="loading" class="gw-empty gw-card" aria-live="polite">
      <RefreshCw class="gw-spinning" :size="26" />
      <h2>正在连接网关</h2>
      <p>读取请求记录和当前配置…</p>
    </div>
    <div v-else-if="!snapshot" class="gw-empty gw-card">
      <Activity :size="28" />
      <h2>暂时无法读取网关</h2>
      <p>连接恢复后即可查看监控与设置，不会覆盖已有服务配置。</p>
      <button
        class="gw-button is-primary"
        type="button"
        :disabled="refreshing"
        @click="reload"
      >
        重新连接
      </button>
    </div>
    <template v-else>
      <MonitorView
        v-show="tab === 'monitor'"
        v-model:auto-refresh="autoRefresh"
        :snapshot="snapshot"
        :keys="keys"
        :scope="info?.isolationScope ?? ''"
      /><SettingsView
        v-if="draft"
        v-show="tab === 'settings'"
        v-model:policy="draft"
        :models="models"
        :accounts="accounts"
        :keys="keys"
        :info="info"
        :info-error="infoError"
        :catalog-loading="catalogLoading"
        :saving="saving"
        :dirty="dirty"
        :conflicted="conflicted"
        :busy="busy"
        :stale="!!refreshError"
        :action-error="actionError"
        :notice="notice"
        :version="snapshot.version"
        @save="save"
        @discard="discard"
        @reload-catalogs="loadCatalogs"
      />
    </template>
  </main>
</template>
