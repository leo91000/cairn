<script setup lang="ts">
import type { Provider } from '../../../../packages/contracts/accounts'
import { brands } from '../accounts'
import Icon from './Icon.vue'

// Each coding agent's brand, on a rounded tile sized like an avatar.
withDefaults(defineProps<{ provider: Provider, size?: number }>(), { size: 38 })
</script>

<template>
  <span
    class="grid shrink-0 place-items-center rounded-[30%]"
    :class="brands[provider].tile"
    :style="{ width: `${size}px`, height: `${size}px` }"
    aria-hidden="true"
  >
    <Icon :name="brands[provider].icon" :size="Math.round(size * 0.5)" />
  </span>
</template>
