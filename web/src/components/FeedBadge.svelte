<script lang="ts">
  import { app } from '../lib/app.svelte'
  import { feedOf, feedText } from '../lib/sources'

  // While a sound or screen effect runs: whose sound or screen the light follows.
  const feeds = $derived.by(() => {
    const name = app.state?.effect?.name
    const info = app.effects.find((effect) => effect.name === name)
    return [info?.needs, info?.also].flatMap((kind) => {
      const feed = kind ? feedOf(app.state, kind, app.mode === 'native') : null
      return kind && feed ? [{ kind, feed }] : []
    })
  })
</script>

{#if feeds.length}
  <div class="mt-1 flex max-w-full flex-wrap gap-1.5">
    {#each feeds as { kind, feed } (kind)}
      <p
        class={[
          'inline-flex max-w-full items-center gap-2 rounded-full border px-3 py-1 text-xs font-medium',
          feed.agent ? 'border-accent text-accent' : 'border-line text-muted',
        ]}
        role="status"
      >
        <span class={['size-1.5 shrink-0 rounded-full', feed.agent ? 'bg-accent' : 'bg-muted']}></span>
        <span class="truncate">{feedText(kind, feed)}</span>
      </p>
    {/each}
  </div>
{/if}
