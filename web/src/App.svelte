<script lang="ts">
  import DevicePanel from './components/DevicePanel.svelte'
  import FeedBadge from './components/FeedBadge.svelte'
  import LightPanel from './components/LightPanel.svelte'
  import Icon from './components/Icon.svelte'
  import Login from './components/Login.svelte'
  import Slider from './components/Slider.svelte'
  import SleepTimer from './components/SleepTimer.svelte'
  import { APP, app, Draft } from './lib/app.svelte'
  import { cssColor } from './lib/color'
  import { latest } from './lib/latest'

  const TABS = [
    { id: 'colour', label: 'Light' },
    { id: 'device', label: 'Device' },
  ] as const
  const CARD = 'bg-card border-line rounded-2xl border p-4 sm:p-5'

  let tab = $state<(typeof TABS)[number]['id']>('colour')

  const on = $derived(!!app.state?.on)
  const glow = $derived(cssColor(app.state?.color ?? '#000000ff'))
  // only the app build carries the screen for finding the bulb
  const setupView = import.meta.env.MODE === 'app' ? import('./components/Setup.svelte') : null
  const brightness = new Draft(() => Math.round((app.state?.brightness ?? 1) * 100))
  const pushBrightness = latest((percent: number) => app.send({ cmd: 'brightness', level: percent / 100 }))

  // what the light shows, or will show once the bulb is back
  const showing = $derived.by(() => {
    const state = app.state
    if (!state) return ''
    if (!state.on) return 'Off'
    if (state.effect) return `Effect: ${state.effect.name}`
    if (state.native) return `Built-in: ${state.native.name}`
    return `On · ${Math.round(state.brightness * 100)}%`
  })
  const AWAY = { lock: 'the computer is locked', sleep: 'the computer is asleep', shutdown: 'the computer is off' }
  const away = $derived(app.state?.away ? AWAY[app.state.away] : null)
  const link = $derived(app.link === 'online' ? (app.state?.link ?? 'waiting') : 'offline')
  const summary = $derived(
    {
      offline: app.mode === 'native' ? 'Starting…' : 'Reconnecting to the service…',
      connecting: 'Connecting to the bulb…',
      waiting: 'Bulb out of reach',
    }[
      link as string
    ] ?? (away ? `Resting while ${away}` : showing),
  )
  const lit = $derived(on && link === 'connected')

  function dim(percent: number) {
    brightness.set(percent)
    pushBrightness(percent)
  }
</script>

{#if app.link === 'signed-out'}
  <Login />
{:else if app.link === 'setup' && setupView}
  {#await setupView then { default: Setup }}
    <Setup />
  {/await}
{:else if !app.state}
  <main class="grid min-h-dvh place-content-center gap-4 px-6 text-center">
    <p class="text-muted">{app.mode === 'native' ? 'Starting…' : 'Connecting to the service…'}</p>
    <button type="button" class="text-accent text-sm font-medium underline" onclick={() => app.logout()}>
      {APP ? 'Choose another bulb or service' : 'Enter a token'}
    </button>
  </main>
{:else}
  <div class="mx-auto max-w-5xl px-4 pt-[max(1rem,env(safe-area-inset-top))] pb-28 lg:pb-10">
    <header class="flex items-center justify-between gap-4 py-2">
      <div class="min-w-0">
        <h1 class="text-2xl font-semibold tracking-tight">Bulb</h1>
        <p class="text-muted flex items-center gap-2 text-sm" role="status">
          <span
            class={[
              'size-2 shrink-0 rounded-full',
              link === 'connected' ? 'bg-ok' : link === 'connecting' ? 'bg-accent animate-pulse' : 'bg-danger',
            ]}
          ></span>
          <span class="truncate">{summary}</span>
        </p>
        <FeedBadge />
      </div>
      <button
        type="button"
        class={[
          'border-line grid size-16 shrink-0 place-items-center rounded-full border transition active:scale-95',
          lit ? 'text-ink' : 'bg-card text-muted',
          on && !lit && 'border-dashed',
        ]}
        style:background={lit ? `color-mix(in srgb, ${glow} 30%, var(--color-card))` : undefined}
        style:box-shadow={lit ? `0 0 2.5rem -0.5rem ${glow}` : undefined}
        aria-pressed={on}
        aria-label={on ? 'Turn the light off' : 'Turn the light on'}
        disabled={app.link !== 'online'}
        onclick={() => app.send({ cmd: on ? 'off' : 'on', fade: true })}
      >
        <Icon name="power" class="size-7" />
      </button>
    </header>

    {#if link === 'connecting' || link === 'waiting'}
      <section class="border-line bg-raised mt-2 grid gap-3 rounded-2xl border p-4 text-sm sm:grid-cols-[1fr_auto] sm:items-center">
        <div class="grid gap-1">
          <h2 class="font-semibold">
            {link === 'connecting' ? 'Connecting to the bulb…' : 'The bulb is out of reach'}
          </h2>
          {#if app.state.problem}
            <p class="text-muted break-words">Last attempt: {app.state.problem}</p>
          {/if}
          {#if app.mode === 'native' && app.setup?.device?.bearer === 'ble'}
            <p class="text-muted">
              Over BLE the bulb answers only while it is not connected as a speaker. Disconnect its audio, or choose
              Classic (SPP) under Device.
            </p>
          {/if}
          <p class="text-muted">
            The service keeps trying. Anything set here is kept and applied when the bulb is back; it will
            show: <span class="text-ink font-medium">{showing}</span>.
          </p>
        </div>
        <button
          type="button"
          class="border-line bg-card h-11 rounded-xl border px-4 font-medium disabled:opacity-50"
          disabled={link === 'connecting'}
          onclick={() => app.send({ cmd: 'reconnect' })}
        >
          Try now
        </button>
      </section>
    {/if}

    {#if away && link === 'connected'}
      <section class="border-line bg-raised mt-2 flex flex-wrap items-center justify-between gap-3 rounded-2xl border p-4 text-sm">
        <p class="text-muted">
          The light rests while {away}; it shows <span class="text-ink font-medium">{showing}</span> again when the
          computer is back, or as soon as anything is changed here.
        </p>
        <button
          type="button"
          class="border-line bg-card h-11 rounded-xl border px-4 font-medium"
          onclick={() => app.send({ cmd: 'back' })}
        >
          Show it now
        </button>
      </section>
    {/if}

    <main class="mt-4 grid gap-4 lg:grid-cols-2 lg:items-start" inert={app.link !== 'online'}>
      <div class="grid gap-4">
        <section class={CARD}>
          <Slider
            label="Brightness"
            value={brightness.value}
            min={1}
            max={100}
            shown="{brightness.value}%"
            track="linear-gradient(to right, color-mix(in srgb, {glow} 15%, var(--color-raised)), {glow})"
            oninput={dim}
          />
          <SleepTimer />
        </section>
        <section class={[CARD, tab !== 'device' && 'hidden lg:block']}><DevicePanel /></section>
      </div>
      <section class={[CARD, 'max-lg:order-first', tab !== 'colour' && 'hidden lg:block']}><LightPanel /></section>
    </main>
  </div>

  <nav
    class="bg-card/90 border-line fixed inset-x-0 bottom-0 border-t pb-[env(safe-area-inset-bottom)] backdrop-blur lg:hidden"
    aria-label="Sections"
  >
    <div class="mx-auto grid max-w-md grid-cols-2">
      {#each TABS as item (item.id)}
        <button
          type="button"
          class={['grid h-16 place-items-center content-center gap-1 text-xs font-medium', tab === item.id ? 'text-accent' : 'text-muted']}
          aria-current={tab === item.id ? 'page' : undefined}
          onclick={() => (tab = item.id)}
        >
          <Icon name={item.id} class="size-6" />
          {item.label}
        </button>
      {/each}
    </div>
  </nav>

  {#if app.error}
    <p
      class="bg-ink text-page fixed inset-x-4 bottom-20 z-10 mx-auto max-w-sm rounded-xl px-4 py-3 text-center text-sm shadow-lg lg:bottom-6"
      role="alert"
    >
      {app.error}
    </p>
  {/if}
{/if}
