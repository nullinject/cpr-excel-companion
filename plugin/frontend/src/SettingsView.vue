<script setup lang="ts">
import type { Account, ClientKey, PluginInfo, Policy } from './gateway'
import { Check, RefreshCw, Search } from '@lucide/vue'
import { computed, onMounted, ref } from 'vue'
import { api } from './api'
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
const section = ref('routing')
// 账户级 Excel(BPS) 授权：按账户 ID 保存；未授权时沿用宿主注入的凭据。
interface AuthEntry {
  accountId: string
  expiresAt: number
  expired: boolean
}
const authEntries = ref(new Map<string, AuthEntry>())
const authBusy = ref('')
const authError = ref('')
const authPending = ref<{ accountId: string, deviceAuthId: string, url: string } | null>(null)
const authCode = ref('')
const authCopied = ref(false)

// iframe 里 window.open 会被 sandbox 拦掉，因此链接必须可见、可复制。
async function copyAuthUrl() {
  const url = authPending.value?.url
  if (!url) {
    return
  }
  try {
    await navigator.clipboard.writeText(url)
    authCopied.value = true
  }
  catch {
    const input = document.getElementById('gw-auth-url') as HTMLInputElement | null
    if (input) {
      input.select()
      try {
        document.execCommand('copy')
        authCopied.value = true
      }
      catch {
        authCopied.value = false
      }
    }
  }
}

async function loadAuthStatus() {
  try {
    const status = await api<{ accounts: AuthEntry[] }>('api/auth/status')
    authEntries.value = new Map(status.accounts.map(entry => [entry.accountId, entry]))
  }
  catch {
    authError.value = '无法读取授权状态'
  }
}

function authorized(accountId: string) {
  const entry = authEntries.value.get(accountId)
  return Boolean(entry && !entry.expired)
}

async function toggleAuth(accountId: string, next: boolean) {
  authError.value = ''
  if (!next) {
    authBusy.value = accountId
    try {
      await api('api/auth/logout', { account_id: accountId })
      authPending.value = null
      await loadAuthStatus()
    }
    catch {
      authError.value = '解除授权失败'
    }
    finally {
      authBusy.value = ''
    }
    return
  }
  authBusy.value = accountId
  try {
    const started = await api<{ deviceAuthId: string, url: string }>('api/auth/start', {
      account_id: accountId,
    })
    authPending.value = { accountId, deviceAuthId: started.deviceAuthId, url: started.url }
    authCode.value = ''
    authCopied.value = false
    // sandbox iframe 拦截 window.open，链接由用户复制到浏览器打开
  }
  catch {
    authError.value = '无法开始授权'
  }
  finally {
    authBusy.value = ''
  }
}

async function completeAuth() {
  const pending = authPending.value
  if (!pending) {
    return
  }
  authBusy.value = pending.accountId
  authError.value = ''
  try {
    await api('api/auth/complete', {
      device_auth_id: pending.deviceAuthId,
      user_code: authCode.value.trim(),
    })
    authPending.value = null
    await loadAuthStatus()
  }
  catch (error) {
    authError.value = error instanceof Error ? error.message : '回调处理失败'
  }
  finally {
    authBusy.value = ''
  }
}

function accountTitle(account: { account_id: string, name?: string, email?: string }) {
  return account.email || account.name || `${account.account_id.slice(0, 12)}…`
}
const policyErrorsAsServerError = computed({
  get: () => policy.value.policy_errors_as_server_error ?? false,
  set: (value: boolean) => { policy.value.policy_errors_as_server_error = value },
})
const sections = [
  { id: 'routing', label: '模型路由' },
  { id: 'capacity', label: '执行限制' },
  { id: 'accounts', label: '账户范围' },
  { id: 'connection', label: '连接信息' },
]
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
onMounted(loadAuthStatus)
</script>

<template>
  <section class="gw-settings" aria-label="网关设置">
    <form @submit.prevent="emit('save')">
      <div class="gw-settings-layout">
        <nav class="gw-settings-nav" aria-label="设置分类">
          <button
            v-for="item in sections"
            :key="item.id"
            type="button"
            :aria-pressed="section === item.id"
            :class="{ 'is-current': section === item.id }"
            @click="section = item.id"
          >
            {{ item.label }}
          </button>
        </nav>
        <fieldset class="gw-settings-fields" :disabled="saving">
          <section
            v-if="section === 'routing'"
            aria-labelledby="routing-heading"
          >
            <div class="gw-section-heading">
              <h2 id="routing-heading">
                模型路由
              </h2>
              <label class="gw-check" for="gw-excel-enabled">
                <input
                  id="gw-excel-enabled"
                  v-model="policy.enabled"
                  type="checkbox"
                  role="switch"
                  aria-label="开启 Excel 通道"
                >启用 Excel 通道
              </label>
            </div>
            <p class="gw-section-description">
              适用于命中插件绑定的请求。Client Key 与模型授权在 CPR 中管理。
            </p>
            <p v-if="!policy.enabled" class="gw-inline-note">
              Excel 通道已关闭，模型规则保留；请求使用原生通道。
            </p>
            <RoutingSettings
              :policy="policy"
              :models="models"
              :keys="keys"
              :suffix="info?.excelModelSuffix ?? '-excel'"
              @change="(model, value, keyId) => setChannel(policy, model, value, keyId)"
            />
          </section>
          <section
            v-if="section === 'capacity'"
            aria-labelledby="capacity-heading"
          >
            <div class="gw-section-heading">
              <h2 id="capacity-heading">
                执行限制
              </h2>
            </div>
            <p class="gw-section-description">
              限制 Excel 请求的并发和等待时间，不调整上游额度。
            </p>
            <div class="gw-form-rows">
              <label class="gw-field" for="gw-concurrency">
                <span>最大并发</span>
                <div>
                  <input
                    id="gw-concurrency"
                    v-model.number="policy.concurrency"
                    type="number"
                    min="1"
                    max="1024"
                    step="1"
                    required
                  ><small>同时执行的请求数，1–1024</small>
                </div>
              </label>
              <label class="gw-field" for="gw-policy-server-error">
                <span>Policy 错误按普通服务错误处理</span>
                <div>
                  <input
                    id="gw-policy-server-error"
                    v-model="policyErrorsAsServerError"
                    type="checkbox"
                    role="switch"
                  ><small>默认关闭。开启后，Excel 通道的 policy 错误转换为 server_error，由客户端或宿主现有机制决定重试，可能产生重复请求和额外计费。插件不自行重试。</small>
                </div>
              </label>
              <label class="gw-field" for="gw-overflow">
                <span>并发已满时</span>
                <div>
                  <select id="gw-overflow" v-model="policy.overflow">
                    <option value="queue">排队等待</option>
                    <option value="reject">拒绝新请求</option>
                  </select>
                </div>
              </label>
              <label
                v-if="policy.overflow === 'queue'"
                class="gw-field"
                for="gw-queue-capacity"
              >
                <span>队列上限</span>
                <div>
                  <input
                    id="gw-queue-capacity"
                    v-model.number="policy.queue_capacity"
                    type="number"
                    min="0"
                    max="10000"
                    step="1"
                    required
                  ><small>最多等待的请求数；0 表示不接受排队</small>
                </div>
              </label>
              <label
                v-if="policy.overflow === 'queue'"
                class="gw-field"
                for="gw-timeout"
              >
                <span>等待超时（秒）</span>
                <div>
                  <input
                    id="gw-timeout"
                    v-model.number="seconds"
                    type="number"
                    min="0.001"
                    max="600"
                    step="0.001"
                    required
                  ><small>超过等待时间后结束请求，最长 600 秒</small>
                </div>
              </label>
            </div>
          </section>
          <section
            v-if="section === 'accounts'"
            aria-labelledby="accounts-heading"
          >
            <div class="gw-section-heading">
              <h2 id="accounts-heading">
                账户范围
              </h2>
              <button
                class="gw-button"
                type="button"
                :disabled="catalogLoading"
                @click="emit('reloadCatalogs')"
              >
                <RefreshCw
                  :size="14"
                  :class="{ 'gw-spinning': catalogLoading }"
                />刷新目录
              </button>
            </div>
            <p class="gw-section-description">
              勾选允许进入 Excel 通道的账户，不改变 CPR 中的账户启用状态。
            </p>
            <div class="gw-toolbar">
              <label class="gw-search" for="gw-account-search">
                <Search :size="15" aria-hidden="true" /><input
                  id="gw-account-search"
                  v-model="accountSearch"
                  type="search"
                  aria-label="搜索账户"
                  placeholder="搜索账户 ID 或分组"
                >
              </label><span class="gw-subtle">
                已允许 {{ selectedCount }} / {{ accounts.length }} 个
              </span>
            </div>
            <div class="gw-account-list">
              <div
                v-for="account in accountList"
                :key="account.account_id"
                class="gw-account-row"
              >
                <label
                  class="gw-account"
                  :for="`gw-account-${account.account_id}`"
                >
                  <input
                    :id="`gw-account-${account.account_id}`"
                    type="checkbox"
                    :checked="accountAllowed(policy, account.account_id)"
                    :aria-label="`允许账户 ${accountTitle(account)}`"
                    @change="
                      setAccountAllowed(
                        policy,
                        account.account_id,
                        ($event.target as HTMLInputElement).checked,
                      )
                    "
                  >
                  <span class="gw-account-label">
                    <strong>{{ accountTitle(account) }}</strong><small>
                      {{
                        account.missing
                          ? "规则中存在，目录中未找到"
                          : account.group_ids?.length
                            ? `分组：${account.group_ids.join("、")}`
                            : "未绑定分组"
                      }}
                    </small>
                  </span>
                  <span class="gw-subtle">
                    {{
                      account.missing
                        ? "待核对"
                        : account.enabled
                          ? "已启用"
                          : "已停用"
                    }}
                  </span>
                </label>
                <label
                  class="gw-switch"
                  :class="{ 'is-on': authorized(account.account_id) }"
                  :title="authorized(account.account_id) ? '关闭将解除该账户的 BPS 授权' : '开启后完成一次 ChatGPT 授权（Excel 同款 SIWC），该账户即可使用 Excel 通道'"
                >
                  <input
                    type="checkbox"
                    role="switch"
                    :checked="authorized(account.account_id)"
                    :disabled="authBusy === account.account_id"
                    :aria-label="`授权 ${accountTitle(account)} 使用 BPS 通道`"
                    @change="
                      toggleAuth(
                        account.account_id,
                        ($event.target as HTMLInputElement).checked,
                      )
                    "
                  >
                  <span class="gw-switch-track" aria-hidden="true"><span /></span>
                  <span class="gw-switch-text">
                    {{ authorized(account.account_id) ? "BPS 已授权" : "BPS 未授权" }}
                  </span>
                </label>
              </div>
              <div v-if="!accountList.length" class="gw-empty">
                <p>
                  {{
                    catalogLoading
                      ? "正在读取账户…"
                      : accountSearch
                        ? "没有匹配的账户"
                        : "暂无 OpenAI 账户"
                  }}
                </p>
              </div>
            </div>
            <p v-if="authError" class="gw-auth-error">
              {{ authError }}
            </p>
            <div v-if="authPending" class="gw-auth-dialog" role="dialog" aria-label="完成 BPS 授权">
              <p><strong>第 1 步</strong>：复制下面的授权链接，在浏览器新标签页打开并登录。</p>
              <div class="gw-auth-link">
                <input
                  id="gw-auth-url"
                  :value="authPending.url"
                  readonly
                  spellcheck="false"
                  @focus="($event.target as HTMLInputElement).select()"
                >
                <button type="button" @click="copyAuthUrl">
                  {{ authCopied ? "已复制" : "复制" }}
                </button>
              </div>
              <p>
                <strong>第 2 步</strong>：登录完成后页面会显示一段授权码，把它粘到下面。
              </p>
              <input
                v-model="authCode"
                type="text"
                placeholder="页面上显示的授权码"
                autocomplete="off"
                spellcheck="false"
              >
              <div class="gw-auth-actions">
                <button type="button" :disabled="authBusy === authPending.accountId" @click="completeAuth">
                  完成授权
                </button>
                <button type="button" class="is-ghost" @click="authPending = null">
                  取消
                </button>
              </div>
            </div>
            <p class="gw-route-note">
              {{
                policy.accounts.allow.length
                  ? "当前使用白名单。取消勾选不会扩大原有允许范围。"
                  : "当前允许所有账户，未勾选项除外；新增账户自动纳入。"
              }}
            </p>
          </section>
          <section
            v-if="section === 'connection'"
            aria-labelledby="connection-heading"
          >
            <div class="gw-section-heading">
              <h2 id="connection-heading">
                连接信息
              </h2>
            </div>
            <p class="gw-section-description">
              只读信息，用于核对插件实例和路由配置。
            </p>
            <p v-if="infoError" class="gw-alert is-warning" role="alert">
              {{ infoError }}
            </p>
            <dl class="gw-connection-list">
              <div>
                <dt>隔离范围</dt>
                <dd>{{ info?.isolationScope ?? "未读取" }}</dd>
              </div>
              <div>
                <dt>模型后缀</dt>
                <dd>{{ info?.excelModelSuffix ?? "未读取" }}</dd>
              </div>
              <div>
                <dt>路由模式</dt>
                <dd>
                  {{
                    info?.routingMode === "host_binding"
                      ? "CPR 插件绑定"
                      : (info?.routingMode ?? "未读取")
                  }}
                </dd>
              </div>
              <div>
                <dt>配置版本</dt>
                <dd>{{ version }}</dd>
              </div>
              <div>
                <dt>Client Key 目录</dt>
                <dd>{{ keys.length }} 个</dd>
              </div>
            </dl>
            <p class="gw-route-note">
              连接地址与签名文件仍在插件管理中配置；此页面不读取密钥或账户凭据。
            </p>
          </section>
        </fieldset>
      </div>
      <div
        v-if="dirty || saving || conflicted || actionError || notice"
        class="gw-save-area"
      >
        <div v-if="conflicted" class="gw-alert is-warning" role="alert">
          <span>服务器配置已更新，草稿保留。重新载入后才能保存。</span><button
            class="gw-text-button"
            type="button"
            :disabled="saving || stale"
            @click="emit('discard')"
          >
            重新载入服务器配置
          </button>
        </div>
        <div v-if="actionError" class="gw-alert is-danger" role="alert">
          <span>{{ actionError }} 草稿已保留。</span>
        </div>
        <div v-if="notice && !dirty" class="gw-save-notice" role="status">
          <Check :size="15" />{{ notice }}
        </div>
        <div v-if="dirty || saving" class="gw-save-bar">
          <span>
            {{
              stale
                ? "连接恢复前不可保存"
                : busy
                  ? "请求执行或排队中，结束后可保存"
                  : saving
                    ? "正在保存…"
                    : "有未保存的改动"
            }}
          </span>
          <div class="gw-save-actions">
            <button
              class="gw-button"
              type="button"
              :disabled="saving || stale"
              @click="emit('discard')"
            >
              放弃改动
            </button><button
              class="gw-button is-primary"
              type="submit"
              :disabled="saving || busy || conflicted || stale"
            >
              {{ saving ? "保存中" : "保存设置" }}
            </button>
          </div>
        </div>
      </div>
    </form>
  </section>
</template>
