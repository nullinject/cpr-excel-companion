<script setup lang="ts">
import type { Account, ClientKey, PluginInfo, Policy } from './gateway'
import { Check, RefreshCw, Save, Search } from '@lucide/vue'
import { computed, ref } from 'vue'
import { accountAllowed, setAccountAllowed, setChannel } from './gateway'
import RoutingSettings from './RoutingSettings.vue'

const props = defineProps<{
  models: string[]
  accounts: Account[]
  keys: ClientKey[]
  info: PluginInfo | null
  infoError: string
  catalogLoading: boolean
  saving: boolean
  dirty: boolean
  conflicted: boolean
  busy: boolean
  stale: boolean
  actionError: string
  notice: string
  version: number
}>()
const emit = defineEmits<{ save: [], discard: [], reloadCatalogs: [] }>()
const policy = defineModel<Policy>('policy', { required: true })
const accountSearch = ref('')
const seconds = computed({
  get: () => policy.value.queue_timeout_ms / 1000,
  set: (value: number) => {
    policy.value.queue_timeout_ms = Math.round(value * 1000)
  },
})
const accountList = computed(() => {
  const rows = new Map(
    props.accounts.map(account => [
      account.account_id,
      { ...account, missing: false },
    ]),
  )
  for (const id of [
    ...policy.value.accounts.allow,
    ...policy.value.accounts.deny,
  ]) {
    if (!rows.has(id))
      rows.set(id, { account_id: id, enabled: false, missing: true })
  }
  const needle = accountSearch.value.toLowerCase().trim()
  return [...rows.values()].filter(account =>
    [account.account_id, ...(account.group_ids ?? [])]
      .join(' ')
      .toLowerCase()
      .includes(needle),
  )
})
const selectedCount = computed(
  () =>
    props.accounts.filter(account =>
      accountAllowed(policy.value, account.account_id),
    ).length,
)
const customModelCount = computed(
  () => Object.keys(policy.value.model_channels).length,
)
</script>

<template>
  <section class="gw-settings" aria-label="网关设置">
    <div class="gw-page-heading">
      <div>
        <h2>网关设置</h2>
        <p>模型通道、运行容量与账户范围，在这里统一管理。</p>
      </div>
      <span class="gw-badge" :class="dirty ? 'is-warning' : 'is-neutral'">
        {{ dirty ? "有未保存改动" : "与服务器一致" }}
      </span>
    </div>
    <form @submit.prevent="emit('save')">
      <fieldset class="gw-settings-fields" :disabled="saving">
        <div class="gw-settings-layout">
          <div class="gw-settings-main">
            <section class="gw-card" aria-labelledby="routing-heading">
              <div class="gw-card-heading">
                <div>
                  <span class="gw-section-kicker">01 / ROUTING</span>
                  <h3 id="routing-heading">
                    模型与通道
                  </h3>
                  <p>为相同模型设置统一的默认通道。</p>
                </div>
                <label class="gw-switch" for="gw-excel-enabled">
                  <input
                    id="gw-excel-enabled"
                    v-model="policy.enabled"
                    type="checkbox"
                    role="switch"
                    aria-label="开启 Excel 通道"
                  ><span aria-hidden="true" /><b>
                    {{ policy.enabled ? "已开启" : "已关闭" }}
                  </b>
                </label>
              </div>
              <div v-if="!policy.enabled" class="gw-inline-note">
                Excel 通道已关闭，绑定范围内的请求走原生通道；模型规则仍保留。
              </div>
              <RoutingSettings
                :policy="policy"
                :models="models"
                :suffix="info?.excelModelSuffix ?? '-excel'"
                @change="(model, value) => setChannel(policy, model, value)"
              />
            </section>
            <section class="gw-card" aria-labelledby="capacity-heading">
              <div class="gw-card-heading">
                <div>
                  <span class="gw-section-kicker">02 / CAPACITY</span>
                  <h3 id="capacity-heading">
                    并发与排队
                  </h3>
                  <p>控制 Excel 通道的执行容量，不调整上游额度。</p>
                </div>
              </div>
              <div class="gw-field-grid">
                <label class="gw-field" for="gw-concurrency">
                  <span>Excel 最大并发</span><input
                    id="gw-concurrency"
                    v-model.number="policy.concurrency"
                    type="number"
                    min="1"
                    max="1024"
                    step="1"
                    required
                  ><small>同时执行的 Excel 请求数</small>
                </label><label class="gw-field" for="gw-overflow">
                  <span>容量满时</span><select id="gw-overflow" v-model="policy.overflow">
                    <option value="queue">进入队列等待</option>
                    <option value="reject">直接拒绝请求</option>
                  </select><small>仅影响超过并发上限的新请求</small>
                </label><label
                  v-if="policy.overflow === 'queue'"
                  class="gw-field"
                  for="gw-queue-capacity"
                >
                  <span>队列上限</span><input
                    id="gw-queue-capacity"
                    v-model.number="policy.queue_capacity"
                    type="number"
                    min="0"
                    max="10000"
                    step="1"
                    required
                  ><small>最多允许等待的请求数</small>
                </label><label
                  v-if="policy.overflow === 'queue'"
                  class="gw-field"
                  for="gw-timeout"
                >
                  <span>排队超时（秒）</span><input
                    id="gw-timeout"
                    v-model.number="seconds"
                    type="number"
                    min="0.001"
                    max="600"
                    step="0.001"
                    required
                  ><small>仅限制等待时间，不是响应超时</small>
                </label>
              </div>
            </section>
            <section class="gw-card" aria-labelledby="accounts-heading">
              <div class="gw-card-heading">
                <div>
                  <span class="gw-section-kicker">03 / ACCOUNTS</span>
                  <h3 id="accounts-heading">
                    Excel 账户范围
                    <span class="gw-count">
                      {{ selectedCount }} / {{ accounts.length }}
                    </span>
                  </h3>
                  <p>
                    允许哪些 OpenAI 账户进入 Excel
                    通道；不改变宿主账户启用状态。
                  </p>
                </div>
                <button
                  class="gw-icon-button"
                  type="button"
                  aria-label="刷新账户与 Key 目录"
                  :disabled="catalogLoading"
                  @click="emit('reloadCatalogs')"
                >
                  <RefreshCw
                    :size="16"
                    :class="{ 'gw-spinning': catalogLoading }"
                  />
                </button>
              </div>
              <div class="gw-toolbar">
                <label class="gw-search" for="gw-account-search">
                  <Search :size="16" aria-hidden="true" /><input
                    id="gw-account-search"
                    v-model="accountSearch"
                    type="search"
                    aria-label="搜索账户"
                    placeholder="搜索账户 ID 或分组"
                  >
                </label><span class="gw-subtle">
                  {{
                    policy.accounts.allow.length
                      ? "白名单范围"
                      : "全部账户，排除禁用项"
                  }}
                </span>
              </div>
              <div class="gw-account-list">
                <label
                  v-for="account in accountList"
                  :key="account.account_id"
                  class="gw-account"
                  :for="`gw-account-${account.account_id}`"
                >
                  <input
                    :id="`gw-account-${account.account_id}`"
                    type="checkbox"
                    :checked="accountAllowed(policy, account.account_id)"
                    :aria-label="`允许账户 ${account.account_id}`"
                    @change="
                      setAccountAllowed(
                        policy,
                        account.account_id,
                        ($event.target as HTMLInputElement).checked,
                      )
                    "
                  ><span class="gw-account-label">
                    <strong>{{ account.account_id }}</strong><small>
                      {{
                        account.missing
                          ? "仅存在于已保存规则，目录中未找到"
                          : account.group_ids?.length
                            ? `分组：${account.group_ids.join("、")}`
                            : "未绑定分组"
                      }}
                    </small>
                  </span><span
                    class="gw-badge"
                    :class="account.enabled ? 'is-success' : 'is-neutral'"
                  >
                    {{
                      account.missing
                        ? "待核对"
                        : account.enabled
                          ? "宿主已启用"
                          : "宿主已停用"
                    }}
                  </span>
                </label>
                <div v-if="!accountList.length" class="gw-empty is-compact">
                  <p>
                    {{
                      catalogLoading
                        ? "正在读取账户…"
                        : accountSearch
                          ? "没有匹配的账户"
                          : "没有可显示的 OpenAI 账户"
                    }}
                  </p>
                </div>
              </div>
            </section>
          </div>
          <aside class="gw-settings-aside">
            <section class="gw-card gw-guide">
              <span class="gw-section-kicker">生效范围</span>
              <h3>先绑定，再分流</h3>
              <p>
                这里的规则作用于已命中 CPR 插件绑定的请求。Key 与模型的授权仍由
                CPR 管理。
              </p>
              <div class="gw-guide-step">
                <span>1</span>
                <div>
                  <b>CPR 插件绑定</b><small>决定哪些 Key / 模型适用</small>
                </div>
              </div>
              <div class="gw-guide-step">
                <span>2</span>
                <div>
                  <b>模型默认通道</b><small>决定请求走 Excel 还是原生</small>
                </div>
              </div>
              <div class="gw-guide-step">
                <span>3</span>
                <div>
                  <b>账户范围与容量</b><small>检查 Excel 准入条件</small>
                </div>
              </div>
              <p class="gw-guide-footnote">
                暂不提供逐 Key × 模型独立通道配置。
              </p>
            </section>
            <section class="gw-card gw-guide">
              <h3>当前草稿</h3>
              <dl class="gw-summary-list">
                <div>
                  <dt>自定义模型规则</dt>
                  <dd>{{ customModelCount }}</dd>
                </div>
                <div>
                  <dt>目录内 Client Key</dt>
                  <dd>{{ keys.length }}</dd>
                </div>
                <div>
                  <dt>配置版本</dt>
                  <dd>v{{ version }}</dd>
                </div>
              </dl>
              <p>
                刷新和切换页面会保留未保存改动。提交时仍会检查运行状态和版本冲突。
              </p>
            </section>
            <details class="gw-card gw-diagnostics">
              <summary>连接与诊断</summary>
              <div>
                <p v-if="infoError" class="gw-danger-text">
                  {{ infoError }}
                </p>
                <dl>
                  <dt>插件隔离范围</dt>
                  <dd>{{ info?.isolationScope ?? "未读取" }}</dd>
                  <dt>Excel 模型后缀</dt>
                  <dd>{{ info?.excelModelSuffix ?? "未读取" }}</dd>
                  <dt>路由模式</dt>
                  <dd>
                    {{
                      info?.routingMode === "host_binding"
                        ? "宿主绑定"
                        : (info?.routingMode ?? "未读取")
                    }}
                  </dd>
                </dl>
                <p>
                  连接地址与敏感凭据仍在插件配置中管理；页面不会读取密钥或账户凭据。
                </p>
              </div>
            </details>
          </aside>
        </div>
      </fieldset>
      <div class="gw-save-area">
        <div v-if="conflicted" class="gw-alert is-warning" role="alert">
          <strong>服务器配置已更新</strong><span>
            你的草稿仍保留。请重新载入服务器配置后再编辑，避免覆盖他人修改。
          </span><button
            class="gw-text-button"
            type="button"
            :disabled="saving || stale"
            @click="emit('discard')"
          >
            重新载入服务器配置
          </button>
        </div>
        <div v-if="actionError" class="gw-alert is-danger" role="alert">
          <strong>设置未保存</strong><span>{{ actionError }} 草稿已保留。</span>
        </div>
        <div v-if="notice && !dirty" class="gw-alert is-success" role="status">
          <Check :size="17" /><span>{{ notice }}</span>
        </div>
        <div class="gw-save-bar">
          <div>
            <strong>
              {{ saving ? "正在保存…" : dirty ? "改动尚未生效" : "所有改动已保存" }}
            </strong><small>
              {{
                stale
                  ? "连接恢复前暂不可保存"
                  : busy
                    ? "有执行或排队请求，请待请求结束后保存"
                    : dirty
                      ? "保存后应用；刷新不会丢失草稿"
                      : "修改配置后在这里提交"
              }}
            </small>
          </div>
          <div class="gw-save-actions">
            <button
              class="gw-button"
              type="button"
              :disabled="!dirty || saving || stale"
              @click="emit('discard')"
            >
              放弃改动
            </button><button
              class="gw-button is-primary"
              type="submit"
              :disabled="!dirty || saving || busy || conflicted || stale"
            >
              <Save :size="16" />{{ saving ? "保存中" : "保存设置" }}
            </button>
          </div>
        </div>
      </div>
    </form>
  </section>
</template>
