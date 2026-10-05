import { Connection, type EffectInfo, type Reply, type State } from './api'

const TOKEN_KEY = 'chsmartbulb.token'
const HOLD = 800 // ms a control keeps its own value after the user last moved it

function remembered(): string {
  try {
    return localStorage.getItem(TOKEN_KEY) ?? ''
  } catch {
    return ''
  }
}

function remember(token: string | null): void {
  try {
    if (token === null) localStorage.removeItem(TOKEN_KEY)
    else localStorage.setItem(TOKEN_KEY, token)
  } catch {
    // private browsing: the token lasts as long as the page
  }
}

class App {
  link = $state<'signed-out' | 'connecting' | 'online' | 'offline'>('signed-out')
  state = $state<State | null>(null)
  effects = $state<EffectInfo[]>([])
  native = $state<{ names: string[]; speed: [number, number] }>({ names: [], speed: [0, 15] })
  loginError = $state('')
  error = $state('')
  signedIn = $state(false) // with a token, as opposed to a service that asks for none

  #connection: Connection | null = null
  #errorTimer: ReturnType<typeof setTimeout> | undefined

  constructor() {
    this.#connect(remembered())
    document.addEventListener('visibilitychange', () => {
      if (!document.hidden) this.#connection?.hurry()
    })
  }

  login(token: string): void {
    this.loginError = ''
    this.#connect(token)
  }

  logout(): void {
    this.#connection?.close()
    this.#connection = null
    remember(null)
    this.state = null
    this.link = 'signed-out'
  }

  /** Send one request; a refusal is shown to the user and returned. */
  async send(message: Record<string, unknown>): Promise<Reply> {
    const reply = (await this.#connection?.request(message)) ?? { ok: false, error: 'not connected' }
    if (!reply.ok) this.#report(reply.error ?? 'request failed')
    return reply
  }

  /** Like `send`, for questions whose failure the caller shows in its own way. */
  async ask(message: Record<string, unknown>): Promise<Reply> {
    return (await this.#connection?.request(message)) ?? { ok: false, error: 'not connected' }
  }

  #connect(token: string): void {
    this.#connection?.close()
    this.link = 'connecting'
    const scheme = location.protocol === 'https:' ? 'wss' : 'ws'
    this.#connection = new Connection(`${scheme}://${location.host}/ws`, token, {
      online: (state) => {
        if (token) remember(token)
        this.signedIn = token !== ''
        this.state = state
        this.link = 'online'
        void this.#describe()
      },
      state: (state) => (this.state = state),
      offline: () => (this.link = this.state ? 'offline' : 'connecting'),
      denied: () => {
        remember(null)
        this.state = null
        this.link = 'signed-out'
        this.loginError = token ? 'That token was refused.' : ''
      },
    })
  }

  async #describe(): Promise<void> {
    const reply = await this.ask({ cmd: 'effects' })
    if (!reply.ok) return
    this.effects = reply.effects as EffectInfo[]
    this.native = reply.native as typeof this.native
  }

  #report(message: string): void {
    this.error = message
    clearTimeout(this.#errorTimer)
    this.#errorTimer = setTimeout(() => (this.error = ''), 4000)
  }
}

export const app = new App()

/**
 * A control's value: follows `read()`, except for a moment after the user moved
 * it, so the echo of their own older values cannot drag the control back.
 */
export class Draft<T> {
  value = $state() as T
  #read: () => T
  #heldUntil = 0
  #timer: ReturnType<typeof setTimeout> | undefined

  constructor(read: () => T) {
    this.#read = read
    this.value = read()
    $effect(() => {
      const fresh = read()
      if (performance.now() >= this.#heldUntil) this.value = fresh
    })
  }

  set(value: T): void {
    this.value = value
    this.#heldUntil = performance.now() + HOLD
    clearTimeout(this.#timer)
    this.#timer = setTimeout(() => (this.value = this.#read()), HOLD + 50)
  }
}
