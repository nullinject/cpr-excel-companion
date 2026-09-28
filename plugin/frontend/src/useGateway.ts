import type {
  Account,
  ClientKey,
  PluginInfo,
  Policy,
  Snapshot,
} from './gateway'
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { api, catalog } from './api'
import { clonePolicy } from './gateway'

export function useGateway() {
  const snapshot = ref<Snapshot | null>(null)
  const draft = ref<Policy | null>(null)
  const savedPolicy = ref<Policy | null>(null)
  const editVersion = ref<number | null>(null)
  const keys = ref<ClientKey[]>([])
  const accounts = ref<Account[]>([])
  const info = ref<PluginInfo | null>(null)
  const loading = ref(true)
  const refreshing = ref(false)
  const saving = ref(false)
  const catalogLoading = ref(false)
  const autoRefresh = ref(true)
  const refreshError = ref('')
  const actionError = ref('')
  const catalogErrors = ref<string[]>([])
  const infoError = ref('')
  const notice = ref('')
  const lastUpdated = ref<number | null>(null)
  const dirty = computed(
    () =>
      !!draft.value
      && JSON.stringify(draft.value) !== JSON.stringify(savedPolicy.value),
  )
  const conflicted = computed(
    () =>
      !!snapshot.value
      && editVersion.value !== null
      && snapshot.value.version !== editVersion.value,
  )
  const busy = computed(
    () => (snapshot.value?.active ?? 0) + (snapshot.value?.waiting ?? 0) > 0,
  )
  const message = (error: unknown) =>
    error instanceof Error ? error.message : '请求未完成，请重试。'
  function adopt(policy: Policy, version: number) {
    savedPolicy.value = clonePolicy(policy)
    draft.value = clonePolicy(policy)
    editVersion.value = version
  }
  async function refresh() {
    if (refreshing.value || saving.value)
      return
    refreshing.value = true
    try {
      const data = await api<Snapshot>('api/snapshot')
      if (
        !data.policy
        || !Array.isArray(data.records)
        || !Number.isInteger(data.version)
      ) {
        throw new Error('桥接返回的状态不完整。')
      }
      if (snapshot.value && data.version < snapshot.value.version)
        return
      snapshot.value = data
      if (!draft.value || !dirty.value)
        adopt(data.policy, data.version)
      lastUpdated.value = Date.now()
      refreshError.value = ''
    }
    catch (error) {
      refreshError.value = message(error)
    }
    finally {
      refreshing.value = false
      loading.value = false
    }
  }
  async function loadCatalogs() {
    if (catalogLoading.value)
      return
    catalogLoading.value = true
    const results = await Promise.allSettled([
      catalog<ClientKey>('api/keys'),
      catalog<Account>('api/accounts'),
    ])
    const errors: string[] = []
    const [keyResult, accountResult] = results
    if (keyResult.status === 'fulfilled')
      keys.value = keyResult.value
    else errors.push(`Client Key 列表：${message(keyResult.reason)}`)
    if (accountResult.status === 'fulfilled')
      accounts.value = accountResult.value
    else errors.push(`账户列表：${message(accountResult.reason)}`)
    catalogErrors.value = errors
    catalogLoading.value = false
  }
  async function loadInfo() {
    try {
      info.value = await api<PluginInfo>('api/plugin-info')
      infoError.value = ''
    }
    catch (error) {
      infoError.value = message(error)
    }
  }
  function discard() {
    if (!snapshot.value)
      return
    adopt(snapshot.value.policy, snapshot.value.version)
    actionError.value = ''
    notice.value = ''
  }
  async function save() {
    if (
      !draft.value
      || saving.value
      || !dirty.value
      || busy.value
      || conflicted.value
      || refreshError.value
    ) {
      return
    }
    const submitted = clonePolicy(draft.value)
    saving.value = true
    actionError.value = ''
    notice.value = ''
    try {
      const result = await api<{ version: number }>('api/policy', {
        policy: submitted,
        expected_version: editVersion.value,
      })
      if (!Number.isInteger(result.version))
        throw new Error('保存响应缺少版本信息，请刷新确认。')
      adopt(submitted, result.version)
      if (snapshot.value) {
        snapshot.value = {
          ...snapshot.value,
          policy: clonePolicy(submitted),
          version: result.version,
        }
      }
      notice.value = '设置已保存并生效。'
    }
    catch (error) {
      actionError.value = message(error)
    }
    finally {
      saving.value = false
    }
  }
  const beforeUnload = (event: BeforeUnloadEvent) => {
    if (dirty.value) {
      event.preventDefault()
      event.returnValue = ''
    }
  }
  let timer: ReturnType<typeof setInterval> | undefined
  onMounted(() => {
    void refresh()
    void loadCatalogs()
    void loadInfo()
    timer = setInterval(() => {
      if (autoRefresh.value && !document.hidden && !saving.value)
        void refresh()
    }, 5000)
    window.addEventListener('beforeunload', beforeUnload)
  })
  onUnmounted(() => {
    if (timer)
      clearInterval(timer)
    window.removeEventListener('beforeunload', beforeUnload)
  })
  return {
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
  }
}
