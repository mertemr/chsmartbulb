<script lang="ts">
  // In the app: offer this device's link to the bulb to the network, as
  // chsmartbulbd does with --listen and --web.
  import { native, type ShareStatus } from '../lib/native'

  let share = $state<ShareStatus | null>(null)
  let busy = $state(false)
  let problem = $state('')

  const host = $derived(share?.addresses[0] ?? 'this-device')
  const page = $derived(share ? `http://${host}:${share.webPort}` : '')
  const port = $derived(share && share.linesPort !== 8377 ? `:${share.linesPort}` : '')

  async function load() {
    try {
      share = await native.shareStatus()
    } catch (error) {
      problem = String(error)
    }
  }

  async function change(enabled: boolean, renew = false) {
    busy = true
    problem = ''
    try {
      share = await native.setSharing(enabled, renew)
    } catch (error) {
      problem = String(error)
    } finally {
      busy = false
    }
  }

  $effect(() => {
    void load()
  })
</script>

<div class="grid gap-3">
  <div class="flex items-center justify-between gap-4">
    <div>
      <h2 class="font-semibold">Share on the network</h2>
      <p class="text-muted text-sm">Other devices control the bulb through this one, and computers can feed it their sound or screen.</p>
    </div>
    <label class="flex shrink-0 items-center gap-2 text-sm font-medium">
      <input
        type="checkbox"
        class="size-5"
        checked={share?.enabled ?? false}
        disabled={busy || !share}
        onchange={(event) => change(event.currentTarget.checked)}
      />
      {share?.enabled ? 'On' : 'Off'}
    </label>
  </div>

  {#if share?.problem || problem}
    <p class="text-danger text-sm break-words" role="alert">{share?.problem ?? problem}</p>
  {/if}

  {#if share?.enabled && share.token}
    <dl class="bg-raised grid gap-2 rounded-xl p-3 text-sm">
      <div class="grid gap-0.5">
        <dt class="text-muted">In a browser</dt>
        <dd class="font-mono break-all">{page}</dd>
      </div>
      <div class="grid gap-0.5">
        <dt class="text-muted">Token</dt>
        <dd class="font-mono break-all select-all">{share.token}</dd>
      </div>
      <div class="grid gap-0.5">
        <dt class="text-muted">From a computer (the Python package)</dt>
        <dd>
          <pre class="bg-card border-line overflow-x-auto rounded-lg border p-2 text-xs">chsmartbulb --host {host}{port} --token {share.token} audio-agent
chsmartbulb --host {host}{port} --token {share.token} screen-agent</pre>
        </dd>
      </div>
    </dl>
    <p class="text-muted text-sm">
      Nothing is encrypted: keep this to a network you trust.
      <button type="button" class="text-accent font-medium" disabled={busy} onclick={() => change(true, true)}>
        New token
      </button>
    </p>
  {/if}
</div>
