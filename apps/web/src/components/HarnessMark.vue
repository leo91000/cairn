<script setup lang="ts">
import type { Provider } from '../../../../packages/contracts/accounts'
import { providers } from '../../../../packages/contracts/accounts'
import { brands } from '../accounts'
import Icon from './Icon.vue'

// The coding agent of a conversation's or mission's latest turn, in its brand colour.
withDefaults(defineProps<{ provider: Provider, size?: number }>(), { size: 12 })
</script>

<template>
  <span
    class="inline-grid shrink-0 place-items-center"
    :class="brands[provider].text"
    :title="providers[provider].label"
    :data-harness="provider"
  >
    <Icon :name="brands[provider].icon" :size="size" />
  </span>
</template>
