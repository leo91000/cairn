<script setup lang="ts">
import { computed } from 'vue'
import { ruleGraphPorts } from '../rule-graph'

const props = withDefaults(defineProps<{ kind?: 'straight' | 'split' | 'merge', leftWidth?: number, rightWidth?: number }>(), { kind: 'straight', leftWidth: 240, rightWidth: 240 })
const ports = computed(() => ruleGraphPorts(props.leftWidth, props.rightWidth))
</script>

<template>
  <div class="graph-link" :class="`graph-link-${kind}`">
    <svg viewBox="0 0 100 64" preserveAspectRatio="none" aria-hidden="true">
      <path v-if="kind === 'split'" :d="`M50 0 V20 M${ports.left} 64 V20 H${ports.right} V64`" />
      <path v-else-if="kind === 'merge'" :d="`M${ports.left} 0 V28 H${ports.right} V0 M50 28 V64`" />
      <path v-else d="M50 0 V64" />
    </svg>
    <template v-if="kind === 'split'">
      <span class="graph-edge-label edge-then" :style="{ left: `${ports.left}%` }">Then</span>
      <span class="graph-edge-label edge-otherwise" :style="{ left: `${ports.right}%` }">Otherwise</span>
      <i class="graph-arrow arrow-then" :style="{ left: `${ports.left}%` }" /><i class="graph-arrow arrow-otherwise" :style="{ left: `${ports.right}%` }" />
    </template>
    <i v-else class="graph-arrow arrow-center" />
  </div>
</template>

<style scoped>
.graph-link { position: relative; height: 64px; flex-shrink: 0; color: var(--color-muted); }
.graph-link svg { display: block; width: 100%; height: 100%; overflow: visible; }
.graph-link path { fill: none; stroke: currentColor; stroke-width: 1.6px; vector-effect: non-scaling-stroke; }
.graph-link-straight { height: 32px; }
.graph-arrow { position: absolute; bottom: 1px; width: 7px; height: 7px; border-right: 2px solid currentColor; border-bottom: 2px solid currentColor; transform: translateX(-50%) rotate(45deg); }
.arrow-then { left: 25%; }.arrow-otherwise { left: 75%; }.arrow-center { left: 50%; }
.graph-edge-label { position: absolute; top: 24px; transform: translateX(-50%); padding: 2px 8px; border-radius: 20px; background: var(--color-canvas); font-size: 11px; font-weight: 700; white-space: nowrap; }
.edge-then { left: 25%; color: var(--color-success); }.edge-otherwise { left: 75%; }
</style>
