<script lang="ts">
  import type { ParamSchema, Value } from '../lib/api'
  import { rgbOnly } from '../lib/color'
  import Icon from './Icon.svelte'
  import Slider from './Slider.svelte'

  type Props = { name: string; schema: ParamSchema; value: Value; onchange: (value: Value) => void }

  const MOST_COLORS = 8
  const WELL = 'border-line size-10 cursor-pointer rounded-lg border bg-transparent p-0.5 disabled:opacity-40'

  let { name, schema, value, onchange }: Props = $props()
  const id = $props.id()
  const label = $derived(name.charAt(0).toUpperCase() + name.slice(1).replaceAll('_', ' '))
  const list = $derived(Array.isArray(value) ? value : [])

  function decimals(step: number): number {
    return (String(step).split('.')[1] ?? '').length
  }

  function replace(index: number, color: string) {
    onchange(list.map((item, i) => (i === index ? color : item)))
  }
</script>

{#if schema.type === 'number'}
  <Slider
    {label}
    value={Number(value)}
    min={schema.min}
    max={schema.max}
    step={schema.step}
    shown={Number(value).toFixed(decimals(schema.step))}
    oninput={onchange}
  />
{:else if schema.type === 'color'}
  <div class="flex min-h-11 items-center justify-between gap-3">
    <label for={id} class="text-sm font-medium">{label}</label>
    <div class="flex items-center gap-3">
      {#if schema.optional}
        <label class="text-muted flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            class="size-4"
            checked={value === null}
            onchange={(event) => onchange(event.currentTarget.checked ? null : '#ff0000')}
          />
          Automatic
        </label>
      {/if}
      <input
        {id}
        type="color"
        class={WELL}
        disabled={value === null}
        value={rgbOnly(typeof value === 'string' ? value : '#ff0000')}
        oninput={(event) => onchange(event.currentTarget.value)}
      />
    </div>
  </div>
{:else}
  <fieldset>
    <legend class="text-sm font-medium">{label}</legend>
    <div class="mt-2 flex flex-wrap items-center gap-2">
      {#each list as color, index (index)}
        <div class="relative">
          <input
            type="color"
            class={WELL}
            aria-label="{label} {index + 1}"
            value={rgbOnly(color)}
            oninput={(event) => replace(index, event.currentTarget.value)}
          />
          {#if list.length > 1}
            <button
              type="button"
              class="bg-raised border-line absolute -top-2 -right-2 grid size-5 place-items-center rounded-full border"
              aria-label="Remove colour {index + 1}"
              onclick={() => onchange(list.filter((_, i) => i !== index))}
            >
              <Icon name="close" class="size-3" />
            </button>
          {/if}
        </div>
      {/each}
      {#if list.length < MOST_COLORS}
        <button
          type="button"
          class="border-line text-muted grid size-10 place-items-center rounded-lg border border-dashed"
          aria-label="Add a colour"
          onclick={() => onchange([...list, '#ffffff'])}
        >
          <Icon name="plus" class="size-4" />
        </button>
      {/if}
    </div>
  </fieldset>
{/if}
