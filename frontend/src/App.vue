<template>
  <div class="min-h-screen bg-neutral-950 text-neutral-100 font-sans">
    <header class="border-b border-neutral-800 px-6 py-3 flex items-center gap-3">
      <router-link to="/" class="text-lg font-semibold tracking-tight">weaver</router-link>
      <span class="text-neutral-500 text-sm">agent workstreams</span>
    </header>
    <main class="px-6 py-5 max-w-6xl mx-auto">
      <router-view />
    </main>
  </div>
</template>
