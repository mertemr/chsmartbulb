<script lang="ts">
  import type { EffectInfo, Value } from '../lib/api'
  import { app, Draft } from '../lib/app.svelte'
  import { latest } from '../lib/latest'
  import ParamField from './ParamField.svelte'
  import SourceNote from './SourceNote.svelte'

  type Params = Record<string, Value>

  // The effects that follow `needs`: nothing (they run on time alone), the sound or the screen.
  let { needs }: { needs: EffectInfo['needs'] } = $props()

  const effects = $derived(app.effects.filter((effect) => effect.needs === needs))
  const active = $derived(app.state?.effect?.name ?? null)
  const info = $derived(effects.find((effect) => effect.name === active) ?? null)
  const values = new Draft<Params>(() => ({ ...info?.params, ...app.state?.effect?.params }))
  const changed = $derived(
    !!info &&
      Object.keys(info.params).some((key) => JSON.stringify(values.value[key]) !== JSON.stringify(info.params[key])),
  )

  // what each effect was last set to, for coming back to it; kept by the browser, so a
  // custom effect survives a visit to another one
  const SETTINGS_KEY = 'chsmartbulb.effects'
  const settings = new Map<string, Params>(recalled())

  function recalled(): [string, Params][] {
    try {
      return Object.entries(JSON.parse(localStorage.getItem(SETTINGS_KEY) ?? '{}'))
    } catch {
      return []
    }
  }

  function keep(name: string, params: Params) {
    settings.set(name, params)
    try {
      localStorage.setItem(SETTINGS_KEY, JSON.stringify({ ...Object.fromEntries(recalled()), [name]: params }))
    } catch {
      // private browsing: remembered for this visit only
    }
  }
  const push = latest((request: { name: string; params: Params }) => app.send({ cmd: 'effect', ...request }))

  function start(effect: EffectInfo) {
    if (effect.name !== active) void app.send({ cmd: 'effect', name: effect.name, params: settings.get(effect.name) })
  }

  function apply(next: Params) {
    if (!info) return
    values.set(next)
    keep(info.name, next)
    push({ name: info.name, params: next })
  }
</script>

<div class="grid gap-4">
  {#if needs}
    <SourceNote kind={needs} />
  {/if}
  {#if info?.also}
    <SourceNote kind={info.also} />
  {/if}

  <div class="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-2 xl:grid-cols-3">
    {#each effects as effect (effect.name)}
      <button
        type="button"
        class={[
          'grid content-start gap-1 rounded-xl border p-3 text-left transition active:scale-[0.98]',
          effect.name === active ? 'border-accent bg-accent/15' : 'border-line',
        ]}
        aria-pressed={effect.name === active}
        onclick={() => start(effect)}
      >
        <span class="font-medium capitalize">{effect.name}</span>
        <span class="text-muted text-sm leading-snug">{effect.summary}</span>
      </button>
    {/each}
  </div>

  {#if info}
    <div class="border-line grid gap-3 rounded-xl border p-4">
      <div class="flex min-h-9 items-center justify-between gap-3">
        <h3 class="font-medium"><span class="capitalize">{info.name}</span> settings</h3>
        <button
          type="button"
          class="border-line h-9 shrink-0 rounded-lg border px-3 text-sm font-medium whitespace-nowrap disabled:opacity-40"
          disabled={!changed}
          onclick={() => apply({ ...info.params })}
        >
          Reset to defaults
        </button>
      </div>
      {#each Object.entries(info.schema) as [key, schema] (key)}
        <ParamField
          name={key}
          {schema}
          value={values.value[key] ?? info.params[key] ?? null}
          onchange={(value) => apply({ ...values.value, [key]: value })}
        />
      {/each}
    </div>
  {:else}
    <p class="text-muted text-sm">Choose one to start it; its settings appear here.</p>
  {/if}
</div>
