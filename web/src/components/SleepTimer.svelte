<script lang="ts">
  import { app } from '../lib/app.svelte'

  // The service's sleep timer: the light dims over the chosen time, then goes off.
  const CHOICES = [15, 30, 60, 120]

  const sleep = $derived(app.state?.sleep ?? null)
  // the service says how much is left whenever it tells the state; the page counts on from there
  let ends = $state(0)
  let now = $state(Date.now())
  $effect(() => {
    ends = sleep ? Date.now() + sleep.left * 1000 : 0
    now = Date.now()
  })
  $effect(() => {
    if (!sleep) return
    const ticking = setInterval(() => (now = Date.now()), 5000)
    return () => clearInterval(ticking)
  })
  const left = $derived(Math.max(1, Math.ceil((ends - now) / 60_000)))

  const label = (minutes: number) => (minutes < 60 ? `${minutes} min` : `${minutes / 60} h`)
</script>

<div class="mt-4 grid gap-2">
  <p class="text-muted text-sm" role="status">
    {sleep ? `Dimming, off in ${left} min` : 'Sleep timer: dim slowly, then switch off'}
  </p>
  <div class="flex flex-wrap gap-2" role="group" aria-label="Sleep timer">
    {#each CHOICES as minutes (minutes)}
      <button
        type="button"
        class={[
          'h-10 rounded-lg border px-3 text-sm font-medium transition active:scale-95 disabled:opacity-40',
          sleep?.minutes === minutes ? 'border-accent text-accent bg-card' : 'border-line bg-card text-muted',
        ]}
        aria-pressed={sleep?.minutes === minutes}
        disabled={!app.state?.on}
        onclick={() => app.send({ cmd: 'sleep', minutes })}
      >
        {label(minutes)}
      </button>
    {/each}
    {#if sleep}
      <button
        type="button"
        class="border-line bg-card text-ink h-10 rounded-lg border px-3 text-sm font-medium transition active:scale-95"
        onclick={() => app.send({ cmd: 'sleep', minutes: 0 })}
      >
        Cancel
      </button>
    {/if}
  </div>
</div>
