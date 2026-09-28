<script setup lang="ts">
import type { ChannelSetting, Policy } from './gateway'
import { computed, ref } from 'vue'
import { modelChannel } from './gateway'

const props = defineProps<{
  policy: Policy
  models: string[]
  suffix: string
}>()
const emit = defineEmits<{
  change: [model: string, channel: ChannelSetting]
}>()
const search = ref('')
const selectedModels = ref<string[]>([])
const visibleModels = computed(() =>
  props.models.filter(model =>
    model.toLowerCase().includes(search.value.toLowerCase()),
  ),
)
const channels: { value: ChannelSetting, label: string }[] = [
  { value: 'inherit', label: '跟随客户端' },
  { value: 'excel', label: 'Excel' },
  { value: 'native', label: '原生' },
]
function batch(channel: ChannelSetting) {
  for (const model of selectedModels.value) emit('change', model, channel)
}
function effective(model: string, excel: boolean) {
  return modelChannel(
    props.policy,
    model,
    excel ? model + props.suffix : model,
    excel,
  ) === 'excel'
    ? 'Excel'
    : '原生'
}
</script>

<template>
  <section aria-label="模型默认通道">
    <div class="gw-toolbar">
      <input
        v-model="search"
        type="search"
        placeholder="搜索模型"
        class="gw-route-search"
        aria-label="搜索模型"
      >
      <span v-if="selectedModels.length">
        已选 {{ selectedModels.length }} 项
      </span>
      <button
        v-for="channel in selectedModels.length ? channels : []"
        :key="channel.value"
        class="gw-button is-small"
        type="button"
        @click="batch(channel.value)"
      >
        {{ channel.label }}
      </button>
      <button
        v-if="selectedModels.length"
        class="gw-button is-small"
        type="button"
        @click="selectedModels = []"
      >
        取消选择
      </button>
    </div>
    <div class="gw-table-scroll">
      <table class="gw-table gw-routing">
        <thead>
          <tr>
            <th aria-label="选择模型" />
            <th>模型</th>
            <th>默认通道</th>
            <th>请求原模型名</th>
            <th>请求 {{ suffix }} 后缀名</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="model in visibleModels" :key="model">
            <td>
              <input
                v-model="selectedModels"
                type="checkbox"
                :value="model"
                :aria-label="`选择 ${model}`"
              >
            </td>
            <td>{{ model }}</td>
            <td>
              <select
                :aria-label="`${model} 的默认通道`"
                :value="policy.model_channels[model] ?? 'inherit'"
                @change="
                  emit(
                    'change',
                    model,
                    ($event.target as HTMLSelectElement)
                      .value as ChannelSetting,
                  )
                "
              >
                <option
                  v-for="channel in channels"
                  :key="channel.value"
                  :value="channel.value"
                >
                  {{ channel.label }}
                </option>
              </select>
            </td>
            <td>{{ effective(model, false) }}</td>
            <td>{{ effective(model, true) }}</td>
          </tr>
          <tr v-if="!visibleModels.length">
            <td colspan="5">
              没有匹配的模型
            </td>
          </tr>
        </tbody>
      </table>
    </div>
    <p class="gw-route-note">
      跟随客户端：原模型名走原生，后缀名走
      Excel。预览还须满足账户范围与宿主绑定。
    </p>
  </section>
</template>
