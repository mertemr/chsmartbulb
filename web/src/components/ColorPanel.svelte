<script lang="ts">
  import { app, Draft } from '../lib/app.svelte'
  import { cssColor, mixed, toHex, pure, unmixed } from '../lib/color'
  import { latest } from '../lib/latest'
  import Slider from './Slider.svelte'

  const HUES = 'linear-gradient(to right, #f00, #ff0, #0f0, #0ff, #00f, #f0f, #f00)'
  const SWATCHES = [
    { name: 'Red', color: mixed(0, 0) },
    { name: 'Orange', color: mixed(25, 0) },
    { name: 'Yellow', color: mixed(55, 0) },
    { name: 'Green', color: mixed(120, 0) },
    { name: 'Cyan', color: mixed(180, 0) },
    { name: 'Blue', color: mixed(240, 0) },
    { name: 'Purple', color: mixed(275, 0) },
    { name: 'Pink', color: mixed(320, 0) },
    { name: 'Warm', color: mixed(28, 0.5) },
    { name: 'White', color: mixed(0, 1) },
  ]

  let lastHue = 30 // white has no hue of its own: keep the slider where it was
  const sliders = new Draft(() => {
    const { hue, white } = unmixed(app.state?.color ?? '#000000ff')
    if (hue !== null) lastHue = hue
    return { hue: hue ?? lastHue, white }
  })

  const plain = $derived(!app.state?.effect && !app.state?.native)
  const tint = $derived(cssColor(toHex(pure(sliders.value.hue))))
  const preview = $derived(cssColor(mixed(sliders.value.hue, sliders.value.white)))

  const push = latest((color: string) => app.send({ cmd: 'color', color }))

  function move(part: { hue?: number; white?: number }) {
    const next = { ...sliders.value, ...part }
    lastHue = next.hue
    sliders.set(next)
    push(mixed(next.hue, next.white))
  }

  function pick(color: string) {
    const { hue, white } = unmixed(color)
    sliders.set({ hue: hue ?? lastHue, white })
    void app.send({ cmd: 'color', color, fade: true })
  }
</script>

<div class="grid gap-4">
  <div class="flex items-center gap-4">
    <div
      class="border-line size-14 shrink-0 rounded-2xl border"
      style:background={preview}
      aria-hidden="true"
    ></div>
    <div>
      <h2 class="font-semibold">Colour</h2>
      <p class="text-muted text-sm">
        {plain ? 'Pick a hue, then blend in the white LEDs.' : 'Choosing a colour ends the running effect.'}
      </p>
    </div>
  </div>

  <Slider
    label="Hue"
    value={sliders.value.hue}
    min={0}
    max={359}
    track={HUES}
    oninput={(hue) => move({ hue })}
  />
  <Slider
    label="White"
    value={Math.round(sliders.value.white * 100)}
    min={0}
    max={100}
    shown="{Math.round(sliders.value.white * 100)}%"
    track="linear-gradient(to right, {tint}, #fff)"
    oninput={(percent) => move({ white: percent / 100 })}
  />

  <div class="grid grid-cols-5 gap-2 sm:grid-cols-10">
    {#each SWATCHES as swatch (swatch.name)}
      <button
        type="button"
        class={[
          'border-line aspect-square rounded-xl border transition active:scale-95',
          plain && app.state?.color === swatch.color && 'ring-ink ring-offset-card ring-2 ring-offset-2',
        ]}
        style:background={cssColor(swatch.color)}
        aria-label={swatch.name}
        title={swatch.name}
        aria-pressed={plain && app.state?.color === swatch.color}
        onclick={() => pick(swatch.color)}
      ></button>
    {/each}
  </div>
</div>
