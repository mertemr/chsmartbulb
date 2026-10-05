import { Connection, type EffectInfo, type Handlers, type Link, type Reply, type State } from './api'
import type { Device, Setup } from './native'

const TOKEN_KEY = 'chsmartbulb.token'
const REMOTE_KEY = 'chsmartbulb.remote'
/** Built as the desktop and mobile app, which carries its own service, rather than served by one. */
export const APP = import.meta.env.MODE === 'app'
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

/** The app's side of the IPC; the page a service serves never loads it. */
function loadNative(): Promise<typeof import('./native')> {
  return import.meta.env.MODE === 'app' ? import('./native') : Promise.reject(new Error('only in the app'))
}

/** A service on another machine, as the app remembers it. */
type Remote = { url: string; token: string }

function rememberedRemote(): Remote | null {
  try {
    return JSON.parse(localStorage.getItem(REMOTE_KEY) ?? 'null')
  } catch {
    return null
  }
}

function rememberRemote(remote: Remote | null): void {
  try {
    if (remote === null) localStorage.removeItem(REMOTE_KEY)
    else localStorage.setItem(REMOTE_KEY, JSON.stringify(remote))
  } catch {
    // remembered for this visit only
  }
}

/** `host`, `host:port` or a full URL, as typed, to the service's WebSocket. */
export function socketUrl(address: string): string {
  const text = address.trim()
  if (/^wss?:\/\//.test(text)) return text
  const url = new URL(/^https?:\/\//.test(text) ? text : `http://${text}`)
  if (!url.port && !/^https?:\/\//.test(text)) url.port = '8378'
  return `${url.protocol === 'https:' ? 'wss' : 'ws'}://${url.host}/ws`
}

class App {
  link = $state<'signed-out' | 'setup' | 'connecting' | 'online' | 'offline'>(APP ? 'connecting' : 'signed-out')
  /** Where the service runs: the server of this page, inside the app, or on another machine. */
  mode = $state<'web' | 'native' | 'remote'>(APP ? 'native' : 'web')
  /** The app's bulb and settings, in the app only. */
  setup = $state<Setup | null>(null)
  /** The machine the service runs on, as agents would address it. */
  serviceHost = $state(APP ? '' : location.hostname)
  state = $state<State | null>(null)
  effects = $state<EffectInfo[]>([])
  native = $state<{ names: string[]; speed: [number, number] }>({ names: [], speed: [0, 15] })
  loginError = $state('')
  error = $state('')
  signedIn = $state(false) // with a token, as opposed to a service that asks for none

  #connection: Link | null = null
  #errorTimer: ReturnType<typeof setTimeout> | undefined

  constructor() {
    if (APP) void this.#start()
    else this.#connect(remembered())
    document.addEventListener('visibilitychange', () => {
      if (!document.hidden) this.#connection?.hurry()
    })
  }

  /** The app: a service on another machine if one was chosen, else the bulb it drives itself. */
  async #start(): Promise<void> {
    const remote = rememberedRemote()
    if (remote) {
      this.mode = 'remote'
      this.serviceHost = new URL(socketUrl(remote.url)).hostname
      this.#connect(remote.token, socketUrl(remote.url))
      return
    }
    const { native } = await loadNative()
    this.setup = await native.setup()
    if (this.setup.device) void this.#connectNative()
    else this.link = 'setup'
  }

  async #connectNative(): Promise<void> {
    const { NativeLink } = await loadNative()
    this.#connection?.close()
    this.mode = 'native'
    this.link = 'connecting'
    this.#connection = new NativeLink(this.#handlers(''))
  }

  /** In the app: drive `device` from here. */
  async useDevice(device: Device): Promise<void> {
    const { native } = await loadNative()
    rememberRemote(null)
    await native.useDevice(device)
    this.setup = await native.setup()
    await this.#connectNative()
  }

  /** In the app: go through a service on another machine instead. */
  useRemote(address: string, token: string): void {
    this.loginError = ''
    this.mode = 'remote'
    this.serviceHost = new URL(socketUrl(address)).hostname
    rememberRemote({ url: address, token })
    this.#connect(token, socketUrl(address))
  }

  login(token: string): void {
    this.loginError = ''
    this.#connect(token)
  }

  /** Sign out of a service, or in the app go back to choosing what to drive. */
  async logout(): Promise<void> {
    this.#connection?.close()
    this.#connection = null
    this.state = null
    if (!APP) {
      remember(null)
      this.link = 'signed-out'
      return
    }
    if (this.mode === 'native') {
      const { native } = await loadNative()
      await native.forgetDevice()
      this.setup = await native.setup()
    }
    rememberRemote(null)
    this.link = 'setup'
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

  #connect(token: string, url?: string): void {
    this.#connection?.close()
    this.link = 'connecting'
    const scheme = location.protocol === 'https:' ? 'wss' : 'ws'
    this.#connection = new Connection(url ?? `${scheme}://${location.host}/ws`, token, this.#handlers(token))
  }

  #handlers(token: string): Handlers {
    return {
      online: (state) => {
        if (token && this.mode === 'web') remember(token)
        this.signedIn = token !== ''
        this.state = state
        this.link = 'online'
        void this.#describe()
      },
      state: (state) => (this.state = state),
      offline: () => (this.link = this.state ? 'offline' : 'connecting'),
      denied: () => {
        this.state = null
        this.loginError = token ? 'That token was refused.' : 'The service asks for a token.'
        if (this.mode === 'remote') {
          rememberRemote(null)
          this.link = 'setup'
        } else {
          remember(null)
          this.link = 'signed-out'
        }
      },
    }
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
