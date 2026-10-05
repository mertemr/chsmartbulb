<script lang="ts">
  type Props = {
    label: string
    value: number
    min: number
    max: number
    step?: number
    track?: string
    shown?: string
    oninput: (value: number) => void
  }

  let { label, value, min, max, step = 1, track, shown, oninput }: Props = $props()
  const id = $props.id()
</script>

<div>
  <div class="flex items-baseline justify-between gap-3 text-sm">
    <label for={id} class="font-medium">{label}</label>
    {#if shown !== undefined}
      <output for={id} class="text-muted tabular-nums">{shown}</output>
    {/if}
  </div>
  <input
    {id}
    class="slider -mb-1.5"
    type="range"
    {min}
    {max}
    {step}
    {value}
    style:--track={track}
    oninput={(event) => oninput(event.currentTarget.valueAsNumber)}
  />
</div>
