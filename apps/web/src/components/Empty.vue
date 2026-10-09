<script setup lang="ts">
import { Sparkles } from '../icons'
import Icon from './Icon.vue'

defineProps<{ title: string, description?: string }>()
</script>

<template>
  <div class="empty text-center flex flex-col items-center px-[25px] py-16 phone:px-5.5 phone:py-[45px]">
    <div class="empty-icon grid place-items-center w-16 h-16 rounded-[19px] bg-surface text-muted mb-5.5 border border-line">
      <Icon :name="Sparkles" :size="27" />
    </div>
    <h3>{{ title }}</h3>
    <p v-if="description">
      {{ description }}
    </p>
    <slot />
  </div>
</template>
