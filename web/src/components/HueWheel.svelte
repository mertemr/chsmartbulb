<script lang="ts">
  // A ring of hues with a handle: drag around it, or use the arrow keys.
  type Props = { hue: number; centre: string; onchange: (hue: number) => void }

  const RING = 0.84 // where the handle sits, as a share of the radius
  const HOLE = 0.45 // presses nearer the middle than this are not on the ring

  let { hue, centre, onchange }: Props = $props()
  let wheel: HTMLDivElement
  let dragging = $state(false)

  const angle = $derived((hue * Math.PI) / 180)

  function follow(event: PointerEvent, starting = false) {
    const box = wheel.getBoundingClientRect()
    const x = event.clientX - (box.left + box.width / 2)
    const y = event.clientY - (box.top + box.height / 2)
    if (starting && Math.hypot(x, y) < (box.width / 2) * HOLE) return false
    onchange(Math.round((Math.atan2(x, -y) * 180) / Math.PI + 360) % 360)
    return true
  }

  function press(event: PointerEvent) {
    if (!follow(event, true)) return
    dragging = true
    wheel.setPointerCapture(event.pointerId)
  }

  function key(event: KeyboardEvent) {
    const step = { ArrowRight: 1, ArrowUp: 1, ArrowLeft: -1, ArrowDown: -1, PageUp: 15, PageDown: -15 }[event.key]
    if (step === undefined) return
    event.preventDefault()
    onchange((hue + step * (event.shiftKey ? 10 : 1) + 360) % 360)
  }
</script>

<div
  bind:this={wheel}
  class="relative mx-auto aspect-square w-full max-w-60 touch-none rounded-full select-none"
  role="slider"
  tabindex="0"
  aria-label="Hue"
  aria-valuemin={0}
  aria-valuemax={359}
  aria-valuenow={hue}
  aria-valuetext="{hue} degrees"
  onpointerdown={press}
  onpointermove={(event) => dragging && follow(event)}
  onpointerup={() => (dragging = false)}
  onpointercancel={() => (dragging = false)}
  onkeydown={key}
>
  <div class="ring absolute inset-0 cursor-pointer rounded-full"></div>
  <div
    class="border-line absolute inset-[27%] rounded-full border shadow-inner transition-colors"
    style:background={centre}
  ></div>
  <div
    class="pointer-events-none absolute size-8 -translate-x-1/2 -translate-y-1/2 rounded-full border-4 border-white shadow-[0_1px_5px_rgb(0_0_0/0.5)]"
    style:left="{50 + Math.sin(angle) * RING * 50}%"
    style:top="{50 - Math.cos(angle) * RING * 50}%"
    style:background="hsl({hue} 100% 50%)"
  ></div>
</div>

<style>
  .ring {
    background: conic-gradient(#f00, #ff0, #0f0, #0ff, #00f, #f0f, #f00);
    mask: radial-gradient(closest-side, transparent 67%, #000 68.5%);
  }
</style>
