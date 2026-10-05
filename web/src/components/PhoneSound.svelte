<script lang="ts">
  // In the app: what the sound effects listen to on this device, and where its sound goes.
  import { app } from '../lib/app.svelte'
  import { native, type AudioInput, type AudioRoute } from '../lib/native'

  const INPUTS: { id: AudioInput; name: string; text: string }[] = [
    {
      id: 'playback',
      name: 'What this device plays',
      text: 'Music and videos, wherever they play: this device’s speaker, headphones or another Bluetooth speaker. Android asks once per session to share the audio.',
    },
    { id: 'microphone', name: 'Microphone', text: 'The room: a TV, a record player, a party.' },
  ]

  let route = $state<AudioRoute | null>(null)
  let saving = $state(false)

  const input = $derived(app.setup?.audioInput ?? 'playback')
  const bulb = $derived(app.setup?.device ?? null)
  const bulbPlays = $derived(!!bulb && !!route?.speakers.includes(bulb.address))
  const android = $derived(app.setup?.platform === 'android')

  async function refresh() {
    try {
      route = await native.audioRoute()
    } catch {
      route = null
    }
  }

  async function choose(next: AudioInput) {
    if (next === input || saving) return
    saving = true
    try {
      await native.setAudioInput(next)
      app.setup = await native.setup()
    } finally {
      saving = false
    }
  }

  $effect(() => {
    void refresh()
  })
</script>

<div class="bg-raised grid gap-3 rounded-xl p-3 text-sm">
  <div class="grid gap-2">
    <p class="font-medium">Listen to</p>
    {#each INPUTS as option (option.id)}
      <label class="flex cursor-pointer gap-3">
        <input
          type="radio"
          class="mt-1"
          name="audio-input"
          checked={input === option.id}
          disabled={saving}
          onchange={() => choose(option.id)}
        />
        <span class="grid gap-0.5">
          <span class="font-medium">{option.name}</span>
          <span class="text-muted">{option.text}</span>
        </span>
      </label>
    {/each}
  </div>

  {#if route}
    <div class="border-line grid gap-2 border-t pt-3">
      <p class="font-medium">Where the sound goes</p>
      <p class="text-muted">
        {route.outputs.length ? route.outputs.join(', ') : 'No output reported'}
        {#if bulbPlays}· the bulb is playing it{/if}
      </p>
      <details class="text-muted">
        <summary class="cursor-pointer py-1">Sound on another speaker, light on the bulb</summary>
        <ul class="mt-1 grid list-disc gap-1.5 pl-5">
          <li>
            Connect to the bulb over <strong>BLE</strong> and keep its audio off: in the Bluetooth settings, open the
            bulb and switch off <em>Media audio</em> (or do not connect it as a speaker). Play to the phone or any other
            speaker; the light follows through “What this device plays”.
          </li>
          <li>
            Samsung phones can send one app’s sound elsewhere with <em>Separate app sound</em>, and play on two
            Bluetooth speakers at once with <em>Dual audio</em> (in the media output panel). With the bulb as one of
            them, connect over <strong>Classic (SPP)</strong>.
          </li>
          <li>
            Bluetooth speakers play about 0.2 s late. Raise the effect’s <em>delay</em> until the light hits with the
            beat.
          </li>
          <li>The bulb’s own <em>music</em> mode (under Bulb) follows only the sound it plays itself.</li>
        </ul>
        {#if android}
          <div class="mt-2 flex gap-2">
            <button
              type="button"
              class="border-line bg-card h-10 flex-1 rounded-lg border font-medium"
              onclick={() => native.openSettings('bluetooth')}
            >
              Bluetooth settings
            </button>
            <button
              type="button"
              class="border-line bg-card h-10 flex-1 rounded-lg border font-medium"
              onclick={() => native.openSettings('sound')}
            >
              Sound settings
            </button>
          </div>
        {/if}
      </details>
    </div>
  {/if}
</div>
