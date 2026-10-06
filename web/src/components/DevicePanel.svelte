<script lang="ts">
  import { app } from '../lib/app.svelte'
  import { cssColor } from '../lib/color'
  import { feedOf } from '../lib/sources'

  // only the app build carries the part that offers this device to the network
  const shareView = import.meta.env.MODE === 'app' ? import('./ShareNetwork.svelte') : null

  type Info = { name: string; model: string; version: string }
  type Tone = 'ok' | 'busy' | 'down' | 'idle'

  const DOT: Record<Tone, string> = {
    ok: 'bg-ok',
    busy: 'bg-accent animate-pulse',
    down: 'bg-danger',
    idle: 'bg-muted',
  }

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

  function feed(kind: 'audio' | 'screen'): { tone: Tone; text: string } {
    const from = feedOf(app.state, kind, app.mode === 'native')
    return { tone: from.agent ? 'ok' : 'idle', text: from.agent ? `${from.from} (agent)` : from.from }
  }

  const links = $derived.by((): { name: string; tone: Tone; text: string }[] => {
    const state = app.state
    const online = app.link === 'online'
    const bulb = { connected: 'connected', connecting: 'connecting…', waiting: 'out of reach' }[state?.link ?? 'waiting']
    const others = (state?.watchers ?? 1) - 1
    const bearer = app.mode === 'native' && app.setup?.device ? ` (${app.setup.device.bearer.toUpperCase()})` : ''
    const own = app.mode === 'native'
    return [
      ...(own
        ? []
        : [{ name: 'This page → service', tone: online ? 'ok' : 'busy', text: online ? 'connected' : 'reconnecting…' } as const]),
      {
        name: own ? 'Bluetooth' + bearer : 'Service → bulb',
        tone: state?.link === 'connected' ? 'ok' : state?.link === 'connecting' ? 'busy' : 'down',
        text: bulb,
      },
      { name: 'Sound from', ...feed('audio') },
      { name: 'Screen from', ...feed('screen') },
      {
        name: 'Also watching',
        tone: others ? 'ok' : 'idle',
        text: others ? `${others} other ${others > 1 ? 'clients' : 'client'}` : 'nobody else',
      },
    ]
  })

  const details = $derived([
    ['Name', info?.name ?? '–'],
    ['Model', info?.model ?? '–'],
    ['Firmware', info?.version ?? '–'],
  ])
</script>

<div class="grid gap-5">
  <div>
    <h2 class="font-semibold">Connections</h2>
    <ul class="divide-line mt-1 divide-y text-sm">
      {#each links as link (link.name)}
        <li class="flex min-h-11 items-center gap-3">
          <span class={['size-2 shrink-0 rounded-full', DOT[link.tone]]}></span>
          <span class="text-muted">{link.name}</span>
          <span class="ml-auto text-right font-medium">{link.text}</span>
        </li>
      {/each}
    </ul>
    {#if app.state?.problem}
      <p class="text-muted mt-2 text-sm break-words">Last attempt to reach the bulb: {app.state.problem}</p>
    {/if}
  </div>

  <div>
    <h2 class="font-semibold">Bulb</h2>
    <dl class="divide-line mt-1 divide-y text-sm">
      {#each details as [term, detail] (term)}
        <div class="flex min-h-10 items-center justify-between gap-4">
          <dt class="text-muted">{term}</dt>
          <dd class="text-right font-medium">{detail}</dd>
        </div>
      {/each}
      {#if reported}
        <div class="flex min-h-10 items-center justify-between gap-4">
          <dt class="text-muted">Reports showing</dt>
          <dd class="flex items-center gap-2 font-mono text-xs">
            {reported}
            <span class="border-line size-5 rounded-md border" style:background={cssColor(reported)}></span>
          </dd>
        </div>
      {/if}
    </dl>
    {#if problem && reachable}
      <p class="text-danger mt-2 text-sm" role="alert">{problem}</p>
    {/if}
  </div>

  {#if app.mode === 'native' && shareView}
    {#await shareView then { default: ShareNetwork }}
      <ShareNetwork />
    {/await}
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
    {#if app.mode !== 'web' || app.signedIn}
      <button
        type="button"
        class="border-line h-11 flex-1 rounded-xl border text-sm font-medium"
        onclick={() => app.logout()}
      >
        {app.mode === 'native' ? 'Change bulb' : app.mode === 'remote' ? 'Leave this service' : 'Sign out'}
      </button>
    {/if}
  </div>
</div>
