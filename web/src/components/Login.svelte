<script lang="ts">
  import { app } from '../lib/app.svelte'

  let token = $state('')

  function submit(event: SubmitEvent) {
    event.preventDefault()
    if (token.trim()) app.login(token.trim())
  }
</script>

<main class="mx-auto grid min-h-dvh max-w-sm content-center gap-6 px-6">
  <div>
    <h1 class="text-2xl font-semibold tracking-tight">Bulb</h1>
    <p class="text-muted mt-1 text-sm">Enter the token the service was started with.</p>
  </div>
  <form class="grid gap-3" onsubmit={submit}>
    <!-- gives a password manager something to file the token under -->
    <input type="text" name="username" autocomplete="username" value="chsmartbulb" hidden />
    <label class="grid gap-1.5 text-sm font-medium">
      Token
      <input
        class="border-line bg-card h-12 rounded-xl border px-4 text-base font-normal"
        type="password"
        autocomplete="current-password"
        autocapitalize="off"
        spellcheck="false"
        required
        bind:value={token}
      />
    </label>
    {#if app.loginError}
      <p class="text-danger text-sm" role="alert">{app.loginError}</p>
    {/if}
    <button class="bg-accent text-on-accent h-12 rounded-xl font-semibold" type="submit">Connect</button>
  </form>
</main>
