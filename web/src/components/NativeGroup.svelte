<script lang="ts">
  import { app, Draft } from '../lib/app.svelte'
  import { latest } from '../lib/latest'
  import Slider from './Slider.svelte'

  const DEFAULT_SPEED = 8

  const active = $derived(app.state?.native?.name ?? null)
  const speed = new Draft(() => app.state?.native?.speed ?? DEFAULT_SPEED)
  const [fastest, slowest] = $derived(app.native.speed)

  const push = latest((request: { name: string; speed: number }) => app.send({ cmd: 'native', ...request }))

  function changeSpeed(level: number) {
    speed.set(level)
    if (active) push({ name: active, speed: level })
  }
</script>

<div class="grid gap-4">
  <p class="text-muted text-sm">
    These run inside the bulb, so they go on when the service or this computer is off.
  </p>
  <div class="flex flex-wrap gap-2">
    {#each app.native.names as name (name)}
      <button
        type="button"
        class={[
          'min-h-11 rounded-xl border px-3 text-sm font-medium capitalize transition active:scale-95',
          name === active ? 'border-accent bg-accent/15' : 'border-line',
        ]}
        aria-pressed={name === active}
        onclick={() => app.send({ cmd: 'native', name, speed: speed.value })}
      >
        {name.replaceAll('_', ' ')}
      </button>
    {/each}
  </div>
  {#if active}
    <div class="border-line grid gap-3 rounded-xl border p-4">
      <div class="flex min-h-9 items-center justify-between gap-3">
        <h3 class="font-medium"><span class="capitalize">{active.replaceAll('_', ' ')}</span> settings</h3>
        <button
          type="button"
          class="border-line h-9 shrink-0 rounded-lg border px-3 text-sm font-medium whitespace-nowrap disabled:opacity-40"
          disabled={speed.value === DEFAULT_SPEED}
          onclick={() => changeSpeed(DEFAULT_SPEED)}
        >
          Reset to defaults
        </button>
      </div>
      <!-- the bulb counts 0 as fastest; the slider runs the other way so right means faster -->
      <Slider
        label="Speed"
        value={slowest - speed.value}
        min={fastest}
        max={slowest}
        shown="{slowest - speed.value + 1} of {slowest - fastest + 1}"
        oninput={(level) => changeSpeed(slowest - level)}
      />
    </div>
  {/if}
</div>
