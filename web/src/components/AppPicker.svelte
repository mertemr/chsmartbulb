<script lang="ts">
  import { app } from '../lib/app.svelte'

  // Which program's sound the effects follow on the service's computer, where it can single one
  // out: everything it plays, or one program alone, so that a call or a game does not move the light.
  let playing = $state<string[]>([])

  const offered = $derived(app.state?.localApp !== undefined && (app.state?.agents.audio ?? 0) === 0)
  const chosen = $derived(app.state?.localApp ?? null)
  // the chosen one stays listed while it plays nothing
  const names = $derived(chosen && !playing.includes(chosen) ? [chosen, ...playing] : playing)
  const options = $derived([{ app: null as string | null, label: 'Everything' }, ...names.map((name) => ({ app: name, label: name }))])

  async function refresh() {
    const reply = await app.ask({ cmd: 'apps' })
    if (reply.ok && Array.isArray(reply.apps)) playing = reply.apps as string[]
  }

  async function choose(name: string | null) {
    await app.send({ cmd: 'listen', app: name })
    void refresh()
  }

  $effect(() => {
    if (!offered) return
    void refresh()
    window.addEventListener('focus', refresh)
    return () => window.removeEventListener('focus', refresh)
  })
</script>

{#if offered}
  <div class="grid gap-2">
    <p class="text-muted flex items-baseline justify-between gap-3 text-xs">
      <span>Which program to listen to{chosen && !playing.includes(chosen) ? ` (${chosen} plays nothing now)` : ''}</span>
      <button type="button" class="shrink-0 underline underline-offset-2" title="Look again for programs playing sound" onclick={refresh}>
        Refresh
      </button>
    </p>
    <div class="flex flex-wrap gap-2" role="group" aria-label="Which program to listen to">
      {#each options as option (option.app)}
        <button
          type="button"
          class={[
            'h-10 rounded-lg border px-3 text-sm font-medium transition active:scale-95',
            chosen === option.app ? 'border-accent text-accent bg-card' : 'border-line bg-card text-muted',
          ]}
          aria-pressed={chosen === option.app}
          onclick={() => choose(option.app)}
        >
          {option.label}
        </button>
      {/each}
    </div>
  </div>
{/if}
