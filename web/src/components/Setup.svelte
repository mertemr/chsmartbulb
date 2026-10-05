<script lang="ts">
  // The app's first screen: find the bulb and choose how to reach it, or use a
  // service that already runs on another machine.
  import { app } from '../lib/app.svelte'
  import { native, type Bearer, type Found } from '../lib/native'

  let bearer = $state<Bearer>(app.setup?.device?.bearer ?? 'ble')
  let found = $state<Found[]>([])
  let searching = $state(false)
  let problem = $state('')
  let connecting = $state('')
  let address = $state('')
  let token = $state('')
  let showAll = $state(false)

  const bulbs = $derived(found.filter((device) => device.likely))
  const listed = $derived(showAll || !bulbs.length ? found : bulbs)

  async function search() {
    problem = ''
    searching = true
    try {
      const ready = await native.prepare()
      if (!ready.granted) {
        problem = 'Bluetooth permission was not granted. Allow “Nearby devices” for this app in the system settings.'
        return
      }
      if (!ready.enabled) {
        problem = 'Bluetooth is off.'
        return
      }
      found = await native.scan(4)
      if (!found.length) problem = 'Nothing found. Is the bulb powered, and within reach?'
    } catch (error) {
      problem = String(error)
    } finally {
      searching = false
    }
  }

  async function choose(device: Found) {
    problem = ''
    connecting = device.address
    try {
      await app.useDevice({ address: device.address, name: device.name, bearer })
    } catch (error) {
      problem = String(error)
    } finally {
      connecting = ''
    }
  }

  function remote(event: SubmitEvent) {
    event.preventDefault()
    if (address.trim()) app.useRemote(address.trim(), token.trim())
  }

  function label(device: Found): string {
    return device.name ?? 'Unnamed device'
  }
</script>

<main class="mx-auto grid min-h-dvh max-w-lg content-start gap-6 px-4 pt-[max(2rem,env(safe-area-inset-top))] pb-10">
  <div>
    <h1 class="text-2xl font-semibold tracking-tight">Bulb</h1>
    <p class="text-muted mt-1 text-sm">Find your bulb. This app connects to it over Bluetooth and keeps it connected.</p>
  </div>

  <section class="bg-card border-line grid gap-4 rounded-2xl border p-4">
    <div class="grid gap-2">
      <h2 class="font-semibold">Connect over</h2>
      <div class="bg-raised flex gap-0.5 rounded-xl p-1" role="radiogroup" aria-label="Bluetooth link">
        {#each [['ble', 'BLE'], ['spp', 'Classic (SPP)']] as [value, name] (value)}
          <button
            type="button"
            role="radio"
            aria-checked={bearer === value}
            class={['h-10 flex-1 rounded-lg text-sm font-medium', bearer === value ? 'bg-card shadow-sm' : 'text-muted']}
            onclick={() => (bearer = value as Bearer)}
          >
            {name}
          </button>
        {/each}
      </div>
      <p class="text-muted text-sm">
        {#if bearer === 'ble'}
          For the light only: your sound stays on this device or goes to another speaker. The bulb takes BLE only while
          it is not connected as a speaker.
        {:else}
          When the bulb is also your speaker: the light is controlled next to the audio link. It must be paired first.
        {/if}
      </p>
    </div>

    <button
      type="button"
      class="bg-accent text-on-accent h-12 rounded-xl font-semibold disabled:opacity-60"
      disabled={searching}
      onclick={search}
    >
      {searching ? 'Searching…' : found.length ? 'Search again' : 'Search for the bulb'}
    </button>

    {#if problem}
      <p class="text-danger text-sm break-words" role="alert">{problem}</p>
    {/if}

    {#if listed.length}
      <ul class="divide-line grid divide-y">
        {#each listed as device (device.address)}
          <li>
            <button
              type="button"
              class="flex min-h-14 w-full items-center gap-3 py-2 text-left disabled:opacity-60"
              disabled={!!connecting}
              onclick={() => choose(device)}
            >
              <span class={['size-2.5 shrink-0 rounded-full', device.likely ? 'bg-accent' : 'bg-line']}></span>
              <span class="grid min-w-0 flex-1">
                <span class="truncate font-medium">{label(device)}</span>
                <span class="text-muted truncate font-mono text-xs">{device.address}</span>
              </span>
              <span class="text-muted flex shrink-0 gap-1 text-xs">
                {#if device.bonded}<span class="bg-raised rounded-md px-1.5 py-0.5">paired</span>{/if}
                {#if device.le}<span class="bg-raised rounded-md px-1.5 py-0.5">BLE</span>{/if}
                {#if device.rssi != null}<span class="px-1 py-0.5">{device.rssi} dBm</span>{/if}
              </span>
              {#if connecting === device.address}<span class="text-accent text-sm">…</span>{/if}
            </button>
          </li>
        {/each}
      </ul>
      {#if bulbs.length && bulbs.length < found.length}
        <button type="button" class="text-accent justify-self-start text-sm font-medium" onclick={() => (showAll = !showAll)}>
          {showAll ? 'Show only bulbs' : `Show all ${found.length} devices`}
        </button>
      {/if}
    {/if}
  </section>

  <section class="bg-card border-line grid gap-3 rounded-2xl border p-4">
    <div>
      <h2 class="font-semibold">Or use a computer’s service</h2>
      <p class="text-muted mt-1 text-sm">
        When a computer already holds the bulb with <code>chsmartbulb daemon --web 8378</code>, control it through that.
      </p>
    </div>
    <form class="grid gap-3" onsubmit={remote}>
      <label class="grid gap-1.5 text-sm font-medium">
        Address
        <input
          class="border-line bg-page h-12 rounded-xl border px-4 text-base font-normal"
          placeholder="laptop.local:8378"
          autocapitalize="off"
          spellcheck="false"
          bind:value={address}
        />
      </label>
      <label class="grid gap-1.5 text-sm font-medium">
        Token <span class="text-muted font-normal">(leave empty for a service started with --no-token)</span>
        <input
          class="border-line bg-page h-12 rounded-xl border px-4 text-base font-normal"
          type="password"
          autocapitalize="off"
          spellcheck="false"
          bind:value={token}
        />
      </label>
      {#if app.loginError}
        <p class="text-danger text-sm" role="alert">{app.loginError}</p>
      {/if}
      <button class="border-line h-12 rounded-xl border font-semibold" type="submit">Connect to the service</button>
    </form>
  </section>
</main>
