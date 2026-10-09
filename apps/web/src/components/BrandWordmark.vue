<template>
  <span class="inline-block" role="img" aria-label="Cairn">
    <img
      class="cairn-mark-light"
      src="/brand/cairn-wordmark.svg"
      alt=""
      width="156"
      height="48"
    >
    <img
      class="cairn-mark-dark"
      src="/brand/cairn-wordmark-dark.svg"
      alt=""
      width="156"
      height="48"
    >
  </span>
</template>

<style>
.cairn-mark-dark { display: none; }
:root[data-theme="dark"] .cairn-mark-light { display: none; }
:root[data-theme="dark"] .cairn-mark-dark { display: block; }
</style>
