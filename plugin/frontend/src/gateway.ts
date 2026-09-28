export type Channel = 'excel' | 'native'
export type ChannelSetting = Channel | 'inherit'
export interface Scope {
  allow: string[]
  deny: string[]
}
export interface Policy {
  enabled: boolean
  models: Scope
  accounts: Scope
  client_keys: Scope
  model_channels: Record<string, Channel>
  key_rules: Record<string, { models: Record<string, Channel> }>
  concurrency: number
  overflow: 'queue' | 'reject'
  queue_capacity: number
  queue_timeout_ms: number
}

export function setChannel(
  policy: Policy,
  model: string,
  channel: ChannelSetting,
) {
  if (channel === 'inherit')
    delete policy.model_channels[model]
  else policy.model_channels[model] = channel
}

// 与 Control::enter 的全局通道和模型范围判断一致；账户仍由宿主选择。
export function modelChannel(
  policy: Policy,
  model: string,
  requested: string,
  excel: boolean,
): Channel {
  const allowed
    = !policy.models.deny.includes(requested)
      && (!policy.models.allow.length || policy.models.allow.includes(requested))
  if (!policy.enabled || !allowed)
    return 'native'
  return policy.model_channels[model] ?? (excel ? 'excel' : 'native')
}

export interface RequestRow {
  request_id: string
  model: string
  client_key_id: string | null
  account_id: string | null
  status: string
  started_at_ms: number
  queue_ms: number | null
  finished_at_ms: number | null
  usage: {
    input_tokens?: number | null
    output_tokens?: number | null
    timings?: { first_token_ms?: number | null, latency_ms?: number | null } | null
  } | null
  error: string | null
  error_code: string | null
  upstream_model: string | null
  source: string
  excel: boolean
}
export interface Snapshot {
  policy: Policy
  version: number
  active: number
  waiting: number
  records: RequestRow[]
}
export interface ClientKey {
  id: string
  name: string
  enabled: boolean
}
export interface Account {
  account_id: string
  enabled: boolean
  provider_id?: string
  group_ids?: string[]
}
export interface PluginInfo {
  isolationScope: string
  excelModelSuffix: string
  routingMode: string
}
export interface CatalogPage<T> {
  items: T[]
  next_cursor: string | null
}
export function clonePolicy(policy: Policy): Policy {
  return JSON.parse(JSON.stringify(policy))
}
export function accountAllowed(policy: Policy, id: string) {
  return (
    !policy.accounts.deny.includes(id)
    && (!policy.accounts.allow.length || policy.accounts.allow.includes(id))
  )
}
export function setAccountAllowed(
  policy: Policy,
  id: string,
  allowed: boolean,
) {
  policy.accounts.deny = policy.accounts.deny.filter(item => item !== id)
  if (
    allowed
    && policy.accounts.allow.length
    && !policy.accounts.allow.includes(id)
  ) {
    policy.accounts.allow.push(id)
  }
  if (!allowed)
    policy.accounts.deny.push(id)
  // 保留白名单项：移除最后一个 allow 会把空列表变为允许全部，错误扩大范围。
}
export function rowChannel(row: RequestRow) {
  return row.source === 'unsigned'
    ? 'unsigned'
    : row.excel
      ? 'excel'
      : 'native'
}
export function statusLabel(status: string) {
  return (
    (
      {
        succeeded: '已完成',
        completed: '已完成',
        running: '执行中',
        queued: '排队中',
        failed: '失败',
        incomplete: '响应不完整',
        cancelled: '已取消',
        rejected: '已拒绝',
        rejected_capacity: '容量已满',
      } as Record<string, string>
    )[status] ?? status
  )
}
export function statusTone(status: string) {
  if (['completed', 'succeeded'].includes(status))
    return 'success'
  if (
    ['failed', 'incomplete', 'rejected', 'rejected_capacity'].includes(status)
  )
    return 'danger'
  if (['running', 'queued'].includes(status))
    return 'active'
  return 'neutral'
}
export function formatDuration(ms: number | null) {
  if (ms == null)
    return '—'
  return ms < 1000
    ? `${Math.round(ms)} ms`
    : `${(ms / 1000).toFixed(ms < 10000 ? 1 : 0)} s`
}

export function formatTokens(value: number | null | undefined) {
  return value == null ? '—' : value.toLocaleString('zh-CN')
}

// 分子、分母均使用同一条宿主用量观察；不把整段请求耗时当生成耗时。
export function tokensPerSecond(usage: RequestRow['usage']) {
  const output = usage?.output_tokens
  const first = usage?.timings?.first_token_ms
  const completed = usage?.timings?.latency_ms
  if (output == null || first == null || completed == null || completed <= first)
    return null
  return output * 1000 / (completed - first)
}
