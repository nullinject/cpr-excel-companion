export type Channel = 'excel' | 'native'
export type ChannelSetting = Channel | 'inherit'
export interface Scope { allow: string[], deny: string[] }
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

export function setChannel(policy: Policy, model: string, channel: ChannelSetting) {
  if (channel === 'inherit')
    delete policy.model_channels[model]
  else policy.model_channels[model] = channel
}

// 与 Control::enter 的全局通道和模型范围判断一致；账户仍由宿主选择。
export function modelChannel(policy: Policy, model: string, requested: string, excel: boolean): Channel {
  const allowed = !policy.models.deny.includes(requested)
    && (!policy.models.allow.length || policy.models.allow.includes(requested))
  if (!policy.enabled || !allowed)
    return 'native'
  return policy.model_channels[model] ?? (excel ? 'excel' : 'native')
}
