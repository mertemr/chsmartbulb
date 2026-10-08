<script lang="ts">
  import { app } from '../lib/app.svelte'
  import { feedOf, feedText } from '../lib/sources'

  // While a sound or screen effect runs: whose sound or screen the light follows.
  const kind = $derived.by(() => {
    const name = app.state?.effect?.name
    return app.effects.find((effect) => effect.name === name)?.needs ?? null
  })
  const feed = $derived(kind ? feedOf(app.state, kind, app.mode === 'native') : null)
</script>

{#if kind && feed}
  <p
    class={[
      'mt-1 inline-flex max-w-full items-center gap-2 rounded-full border px-3 py-1 text-xs font-medium',
      feed.agent ? 'border-accent text-accent' : 'border-line text-muted',
    ]}
    role="status"
  >
    <span class={['size-1.5 shrink-0 rounded-full', feed.agent ? 'bg-accent' : 'bg-muted']}></span>
    <span class="truncate">{feedText(kind, feed)}</span>
  </p>
{/if}
