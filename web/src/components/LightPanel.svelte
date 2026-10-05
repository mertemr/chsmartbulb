<script lang="ts">
  import { untrack } from 'svelte'
  import { app } from '../lib/app.svelte'
  import ColourView from './ColourView.svelte'
  import EffectGroup from './EffectGroup.svelte'
  import NativeGroup from './NativeGroup.svelte'

  // What the light does is one of these; choosing something in a mode switches the light to it.
  const MODES = [
    { id: 'colour', label: 'Colour' },
    { id: 'patterns', label: 'Patterns' },
    { id: 'sound', label: 'Sound' },
    { id: 'screen', label: 'Screen' },
    { id: 'bulb', label: 'Bulb' },
  ] as const
  type Mode = (typeof MODES)[number]['id']

  const running = $derived.by((): Mode => {
    const state = app.state
    if (state?.native) return 'bulb'
    if (!state?.effect) return 'colour'
    const needs = app.effects.find((effect) => effect.name === state.effect?.name)?.needs
    return needs === 'audio' ? 'sound' : needs === 'screen' ? 'screen' : 'patterns'
  })

  let shown = $state<Mode>(untrack(() => running))
  $effect(() => {
    shown = running // whoever changes the mode, the page shows it
  })
</script>

<div class="grid gap-5">
  <div class="bg-raised -mx-1 flex gap-0.5 rounded-xl p-1" role="tablist" aria-label="What the light does">
    {#each MODES as mode (mode.id)}
      <button
        type="button"
        role="tab"
        class={[
          'relative h-10 min-w-0 flex-1 rounded-lg px-1 text-[0.8125rem] font-medium transition sm:text-sm',
          shown === mode.id ? 'bg-card text-ink shadow-sm' : 'text-muted',
        ]}
        aria-selected={shown === mode.id}
        onclick={() => (shown = mode.id)}
      >
        {mode.label}
        {#if running === mode.id}
          <span class="bg-accent absolute top-1 right-1 size-1.5 rounded-full" title="Active"></span>
        {/if}
      </button>
    {/each}
  </div>

  <div role="tabpanel">
    {#if shown === 'colour'}
      <ColourView steady={running === 'colour'} />
    {:else if shown === 'patterns'}
      <EffectGroup needs={null} />
    {:else if shown === 'sound'}
      <EffectGroup needs="audio" />
    {:else if shown === 'screen'}
      <EffectGroup needs="screen" />
    {:else}
      <NativeGroup />
    {/if}
  </div>
</div>
