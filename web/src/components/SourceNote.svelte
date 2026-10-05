<script lang="ts">
  import { app } from '../lib/app.svelte'

  // Where the sound or the screen comes from, and how to bring it in from another computer.
  let { kind }: { kind: 'audio' | 'screen' } = $props()

  // only the app build carries the panel about this device's own sound
  const phoneSound = import.meta.env.MODE === 'app' ? import('./PhoneSound.svelte') : null

  const WHAT = { audio: 'sound', screen: 'screen' }
  const agents = $derived(app.state?.agents?.[kind] ?? 0)
  const command = $derived(`chsmartbulb --host ${app.serviceHost}${app.signedIn ? ' --token …' : ''} ${kind}-agent`)
  const listens = $derived(kind === 'audio' && app.setup?.platform === 'android')
</script>

{#if app.mode === 'native'}
  {#if listens && phoneSound}
    {#await phoneSound then { default: PhoneSound }}
      <PhoneSound />
    {/await}
  {:else}
    <p class="bg-raised text-muted rounded-xl p-3 text-sm">
      {#if kind === 'audio'}
        The desktop app does not listen to sound yet. Until it does, run the Python service on this computer, which
        follows its audio, and connect to it from the start screen.
      {:else}
        The screen effect needs a computer: run the Python service there with <code>screen-agent</code> and connect to
        it from the start screen.
      {/if}
    </p>
  {/if}
{:else}
  <div class="bg-raised grid gap-2 rounded-xl p-3 text-sm">
    <p class="flex items-center gap-2 font-medium">
      <span class={['size-2 shrink-0 rounded-full', agents ? 'bg-ok' : 'bg-muted']}></span>
      {#if agents}
        Following the {WHAT[kind]} of another computer
        {#if agents > 1}<span class="text-muted font-normal">({agents} agents)</span>{/if}
      {:else}
        Following the {WHAT[kind]} of the computer the service runs on
      {/if}
    </p>
    <details class="text-muted">
      <summary class="cursor-pointer py-1">Use another computer's {WHAT[kind]}</summary>
      <p class="mt-1">
        Run an agent there. It takes over while it is connected and hands back when it leaves. Agents
        come in through the port the service was given with <code>--listen</code>, not through this page's.
      </p>
      <pre class="bg-card border-line text-ink mt-2 overflow-x-auto rounded-lg border p-2 text-xs">{command}</pre>
    </details>
  </div>
{/if}
