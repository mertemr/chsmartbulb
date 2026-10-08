<script lang="ts">
  import { app } from '../lib/app.svelte'

  // Which monitor the screen effect follows: on the computer of the agent that feeds it, or on the
  // service's own computer while no agent does. Nothing shows for a machine with one monitor.
  type Target = { id: string; name: string; monitors: { index: number; width: number; height: number }[]; monitor: number | null }

  const targets = $derived.by((): Target[] => {
    const state = app.state
    if (!state) return []
    if (state.agents.screen > 0) {
      return (state.agentInfo?.screen ?? [])
        .filter((agent) => agent.monitors.length > 1)
        .map((agent) => ({ ...agent, name: agent.name ?? 'another computer' }))
    }
    const monitors = state.localMonitors ?? []
    return monitors.length > 1
      ? [{ id: 'local', name: 'the service’s computer', monitors, monitor: state.localMonitor ?? null }]
      : []
  })

  async function choose(id: string, index: number) {
    await app.send({ cmd: 'monitor', agent: id, index })
  }
</script>

{#each targets as target (target.id)}
  <div class="grid gap-2">
    <p class="text-muted text-xs">Which screen on {target.name}</p>
    <div class="flex flex-wrap gap-2" role="group" aria-label="Which screen on {target.name}">
      {#each [...target.monitors.map((area) => ({ index: area.index, label: `${area.index} · ${area.width}×${area.height}` })), { index: 0, label: 'All' }] as option (option.index)}
        <button
          type="button"
          class={[
            'h-10 rounded-lg border px-3 text-sm font-medium transition active:scale-95',
            target.monitor === option.index ? 'border-accent text-accent bg-card' : 'border-line bg-card text-muted',
          ]}
          aria-pressed={target.monitor === option.index}
          onclick={() => choose(target.id, option.index)}
        >
          {option.label}
        </button>
      {/each}
    </div>
  </div>
{/each}
