<script lang="ts">
  import { app } from '../lib/app.svelte'
  import { cssColor } from '../lib/color'

  type Info = { name: string; model: string; version: string }

  let info = $state<Info | null>(null)
  let reported = $state<string | null>(null)
  let problem = $state('')

  const reachable = $derived(app.link === 'online' && !!app.state?.connected)

  async function refresh() {
    const [answer, status] = await Promise.all([app.ask({ cmd: 'info' }), app.ask({ cmd: 'status' })])
    info = answer.ok ? (answer as unknown as Info) : null
    problem = answer.ok ? '' : (answer.error ?? '')
    reported = status.ok && typeof status.bulb === 'string' ? status.bulb : null
  }

  $effect(() => {
    if (reachable) void refresh()
  })

  const rows = $derived([
    ['Bulb', { connected: 'connected', connecting: 'connecting…', waiting: 'out of reach' }[app.state?.link ?? 'waiting']],
    ['Name', info?.name ?? '–'],
    ['Model', info?.model ?? '–'],
    ['Firmware', info?.version ?? '–'],
    ['Audio from', app.state?.audio === 'agent' ? 'an agent' : 'this machine'],
    ['Screen from', app.state?.screen === 'agent' ? 'an agent' : 'this machine'],
  ])
</script>

<div class="grid gap-4">
  <h2 class="font-semibold">Device</h2>
  <dl class="divide-line divide-y text-sm">
    {#each rows as [term, detail] (term)}
      <div class="flex min-h-10 items-center justify-between gap-4">
        <dt class="text-muted">{term}</dt>
        <dd class="text-right font-medium">{detail}</dd>
      </div>
    {/each}
    {#if reported}
      <div class="flex min-h-10 items-center justify-between gap-4">
        <dt class="text-muted">Bulb reports</dt>
        <dd class="flex items-center gap-2 font-mono text-xs">
          {reported}
          <span class="border-line size-5 rounded-md border" style:background={cssColor(reported)}></span>
        </dd>
      </div>
    {/if}
  </dl>
  {#if app.state?.problem}
    <p class="text-muted text-sm break-words">Last attempt to connect: {app.state.problem}</p>
  {/if}
  {#if problem && reachable}
    <p class="text-danger text-sm" role="alert">{problem}</p>
  {/if}
  <div class="flex gap-2">
    <button
      type="button"
      class="border-line h-11 flex-1 rounded-xl border text-sm font-medium disabled:opacity-50"
      disabled={!reachable}
      onclick={refresh}
    >
      Refresh
    </button>
    <button type="button" class="border-line h-11 flex-1 rounded-xl border text-sm font-medium" onclick={() => app.logout()}>
      Sign out
    </button>
  </div>
</div>
