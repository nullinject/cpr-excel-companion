<script setup lang="ts">
import type { ChannelSetting, ClientKey, Policy } from './gateway'
import { computed, ref, watch } from 'vue'
import { channelSetting, modelChannel } from './gateway'

const props = defineProps<{
  policy: Policy
  models: string[]
  keys: ClientKey[]
  suffix: string
}>()
const emit = defineEmits<{
  change: [model: string, channel: ChannelSetting, keyId: string | null]
}>()
const search = ref('')
const selectedKey = ref('')
const selectedModels = ref<string[]>([])
watch(selectedKey, () => {
  selectedModels.value = []
})
const keyChoices = computed(() => {
  const known = new Map(props.keys.map(key => [key.id, { ...key, missing: false }]))
  for (const id of Object.keys(props.policy.key_rules)) {
    if (!known.has(id))
      known.set(id, { id, name: id, enabled: false, missing: true })
  }
  return [...known.values()].sort((a, b) => a.name.localeCompare(b.name))
})
const visibleModels = computed(() => props.models.filter(model => model.toLowerCase().includes(search.value.toLowerCase().trim())))
const channels = computed<{ value: ChannelSetting, label: string }[]>(() => [
  { value: 'inherit', label: selectedKey.value ? '继承全局 / 客户端' : '跟随客户端' },
  { value: 'excel', label: 'Excel' },
  { value: 'native', label: '原生' },
])
function change(model: string, channel: ChannelSetting) {
  emit('change', model, channel, selectedKey.value || null)
}
function batch(channel: ChannelSetting) {
  for (const model of selectedModels.value) change(model, channel)
}
function effective(model: string, excel: boolean) {
  return modelChannel(props.policy, model, excel ? model + props.suffix : model, excel, selectedKey.value || null) === 'excel' ? 'Excel' : '原生'
}
</script>

<template>
  <section aria-label="Key × 模型 × 通道矩阵">
    <div class="gw-toolbar">
      <label for="gw-routing-key">
        路由范围
        <select id="gw-routing-key" v-model="selectedKey" aria-label="选择路由 Key">
          <option value="">
            全局模型规则
          </option>
          <option v-for="key in keyChoices" :key="key.id" :value="key.id">
            {{ key.name }} · {{ key.id }}{{ key.missing ? '（目录未找到）' : key.enabled ? '' : '（已停用）' }}
          </option>
        </select>
      </label>
      <input v-model="search" type="search" placeholder="搜索模型" class="gw-route-search" aria-label="搜索模型">
    </div>
    <p class="gw-route-note">
      Key × 模型优先于全局模型规则，再跟随客户端后缀。当前正在编辑{{ selectedKey ? '所选 Key 的覆盖' : '全局规则' }}。
    </p>
    <div v-if="selectedModels.length" class="gw-toolbar">
      <span>已选 {{ selectedModels.length }} 项</span>
      <button v-for="channel in channels" :key="channel.value" class="gw-button is-small" type="button" @click="batch(channel.value)">
        {{ channel.label }}
      </button>
      <button class="gw-button is-small" type="button" @click="selectedModels = []">
        取消选择
      </button>
    </div>
    <div class="gw-table-scroll">
      <table class="gw-table gw-routing">
        <thead>
          <tr>
            <th aria-label="选择模型" />
            <th>基础模型</th>
            <th>{{ selectedKey ? 'Key 通道覆盖' : '全局通道' }}</th>
            <th>请求原模型名</th>
            <th>请求 {{ suffix }} 后缀名</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="model in visibleModels" :key="model">
            <td><input v-model="selectedModels" type="checkbox" :value="model" :aria-label="`选择 ${model}`"></td>
            <td>{{ model }}</td>
            <td>
              <select :aria-label="`${model} 的通道`" :value="channelSetting(policy, model, selectedKey || null)" @change="change(model, ($event.target as HTMLSelectElement).value as ChannelSetting)">
                <option v-for="channel in channels" :key="channel.value" :value="channel.value">
                  {{ channel.label }}
                </option>
              </select>
            </td>
            <td>{{ effective(model, false) }}</td>
            <td>{{ effective(model, true) }}</td>
          </tr>
          <tr v-if="!visibleModels.length">
            <td colspan="5">
              暂无匹配模型；模型来自请求记录和已保存规则，而不是硬编码的可用性名单。
            </td>
          </tr>
        </tbody>
      </table>
    </div>
    <p class="gw-route-note">
      通道选择不授予上游模型权限，不会在 403 后自动换通道。预览仍须满足实际账户范围、CPR 授权与插件绑定；目录缺失或停用的 Key 不会因此获准访问。
    </p>
  </section>
</template>
