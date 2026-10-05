<script lang="ts">
  const PATHS = {
    power: 'M12 3v9 M6.300 6.300a8 8 0 1 0 11.400 0',
    colour: 'M12 3s6 6.200 6 10.500A6 6 0 0 1 6 13.500C6 9.200 12 3 12 3Z',
    effects: 'M11 4l1.800 5.200L18 11l-5.200 1.800L11 18l-1.800-5.200L4 11l5.200-1.800Z M19 16v4 M17 18h4',
    device: 'M8 6h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2Z M9 2v4 M15 2v4 M9 18v4 M15 18v4 M2 9h4 M2 15h4 M18 9h4 M18 15h4',
    stop: 'M7 7h10v10H7Z',
    plus: 'M12 5v14 M5 12h14',
    close: 'M6 6l12 12 M18 6L6 18',
    up: 'M6 15l6-6 6 6',
    down: 'M6 9l6 6 6-6',
  }

  let { name, class: classes = 'size-5' }: { name: keyof typeof PATHS; class?: string } = $props()
</script>

<svg
  class={classes}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="2"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  <path d={PATHS[name]} />
</svg>
