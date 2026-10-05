<script lang="ts">
  import type { ParamSchema, Step, Value } from '../lib/api'
  import { rgbOnly } from '../lib/color'
  import Icon from './Icon.svelte'

  // The colours of a custom effect, in order: each fades in, then holds.
  type Props = {
    schema: Extract<ParamSchema, { type: 'steps' }>
    steps: Step[]
    onchange: (value: Value) => void
  }

  const FIELD = 'border-line bg-card h-10 w-full rounded-lg border px-2 text-sm tabular-nums'
  const BUTTON = 'border-line grid size-8 place-items-center rounded-lg border disabled:opacity-30'

  let { schema, steps, onchange }: Props = $props()

  const total = $derived(steps.reduce((sum, step) => sum + step.hold + step.fade, 0))

  // the sequence as a strip: each step's fade and hold take their share of the width
  const strip = $derived.by(() => {
    if (!total) return 'transparent'
    const stops: string[] = []
    let at = 0
    for (const step of steps) {
      const color = rgbOnly(step.color)
      at += step.fade
      stops.push(`${color} ${(at / total) * 100}%`)
      at += step.hold
      stops.push(`${color} ${(at / total) * 100}%`)
    }
    return `linear-gradient(to right, ${rgbOnly(steps.at(-1)!.color)} 0%, ${stops.join(', ')})`
  })

  function change(index: number, part: Partial<Step>) {
    onchange(steps.map((step, i) => (i === index ? { ...step, ...part } : step)))
  }

  function seconds(event: Event & { currentTarget: HTMLInputElement }): number {
    const value = event.currentTarget.valueAsNumber
    return Number.isFinite(value) ? Math.max(0, value) : 0
  }

  function move(index: number, by: number) {
    const next = [...steps]
    const [step] = next.splice(index, 1)
    next.splice(index + by, 0, step!)
    onchange(next)
  }

  function add() {
    const last = steps.at(-1)
    onchange([...steps, { color: '#ffffff', hold: last?.hold ?? 1, fade: last?.fade ?? 1, ease: last?.ease ?? 'linear' }])
  }
</script>

<fieldset class="grid gap-3">
  <legend class="flex w-full items-baseline justify-between text-sm">
    <span class="font-medium">Steps</span>
    <span class="text-muted tabular-nums">{total.toFixed(1)} s a round</span>
  </legend>

  <div class="border-line h-4 rounded-full border" style:background={strip} aria-hidden="true"></div>

  <ol class="grid gap-2">
    {#each steps as step, index (index)}
      <li class="bg-raised grid grid-cols-[auto_1fr_1fr] items-end gap-2 rounded-xl p-2 sm:grid-cols-[auto_4.5rem_4.5rem_1fr_auto]">
        <input
          type="color"
          class="border-line size-10 cursor-pointer rounded-lg border bg-transparent p-0.5"
          aria-label="Colour of step {index + 1}"
          value={rgbOnly(step.color)}
          oninput={(event) => change(index, { color: event.currentTarget.value })}
        />
        <label class="text-muted grid gap-0.5 text-xs">
          Fade in, s
          <input
            class={FIELD}
            type="number"
            inputmode="decimal"
            min="0"
            step="0.1"
            value={step.fade}
            onchange={(event) => change(index, { fade: seconds(event) })}
          />
        </label>
        <label class="text-muted grid gap-0.5 text-xs">
          Hold, s
          <input
            class={FIELD}
            type="number"
            inputmode="decimal"
            min="0"
            step="0.1"
            value={step.hold}
            onchange={(event) => change(index, { hold: seconds(event) })}
          />
        </label>
        <label class="text-muted col-span-2 grid gap-0.5 text-xs sm:col-span-1">
          Fade shape
          <select
            class={FIELD}
            value={step.ease}
            onchange={(event) => change(index, { ease: event.currentTarget.value })}
          >
            {#each schema.easings as easing (easing)}
              <option value={easing}>{easing.replaceAll('-', ' ')}</option>
            {/each}
          </select>
        </label>
        <div class="flex items-center justify-end gap-1">
          <button type="button" class={BUTTON} aria-label="Move step {index + 1} earlier" disabled={index === 0} onclick={() => move(index, -1)}>
            <Icon name="up" class="size-4" />
          </button>
          <button type="button" class={BUTTON} aria-label="Move step {index + 1} later" disabled={index === steps.length - 1} onclick={() => move(index, 1)}>
            <Icon name="down" class="size-4" />
          </button>
          <button type="button" class={BUTTON} aria-label="Remove step {index + 1}" disabled={steps.length === 1} onclick={() => onchange(steps.filter((_, i) => i !== index))}>
            <Icon name="close" class="size-4" />
          </button>
        </div>
      </li>
    {/each}
  </ol>

  <button
    type="button"
    class="border-line text-muted flex h-10 items-center justify-center gap-2 rounded-xl border border-dashed text-sm font-medium disabled:opacity-40"
    disabled={steps.length >= schema.most}
    onclick={add}
  >
    <Icon name="plus" class="size-4" />
    Add a step
  </button>
</fieldset>
