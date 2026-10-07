<script lang="ts">
  // In the desktop app: what the light does while this computer is locked or asleep.
  import { app } from '../lib/app.svelte'
  import { native, type Away, type AwayLook } from '../lib/native'

  const CHOICES: { value: AwayLook; label: string }[] = [
    { value: null, label: 'Nothing' },
    { value: 'dim', label: 'Dim' },
    { value: 'off', label: 'Off' },
  ]
  const MOMENTS: { key: keyof Away; label: string }[] = [
    { key: 'lock', label: 'When this computer locks' },
    { key: 'sleep', label: 'When it sleeps or shuts down' },
  ]

  let problem = $state('')
  const away = $derived<Away>(app.setup?.away ?? { lock: null, sleep: null })

  async function choose(key: keyof Away, value: AwayLook) {
    problem = ''
    try {
      await native.setAway({ ...away, [key]: value })
      app.setup = await native.setup()
    } catch (error) {
      problem = String(error)
    }
  }
</script>

<div class="grid gap-3">
  <div>
    <h2 class="font-semibold">While you are away</h2>
    <p class="text-muted text-sm">The light comes back as it was when you do, or as soon as anything is changed.</p>
  </div>
  {#each MOMENTS as moment (moment.key)}
    <div class="flex flex-wrap items-center justify-between gap-2 text-sm">
      <span class="text-muted">{moment.label}</span>
      <div class="bg-raised flex gap-0.5 rounded-xl p-1" role="radiogroup" aria-label={moment.label}>
        {#each CHOICES as choice (choice.label)}
          <button
            type="button"
            role="radio"
            aria-checked={away[moment.key] === choice.value}
            class={[
              'h-9 rounded-lg px-3 font-medium',
              away[moment.key] === choice.value ? 'bg-card shadow-sm' : 'text-muted',
            ]}
            onclick={() => choose(moment.key, choice.value)}
          >
            {choice.label}
          </button>
        {/each}
      </div>
    </div>
  {/each}
  {#if problem}
    <p class="text-danger text-sm" role="alert">{problem}</p>
  {/if}
</div>
