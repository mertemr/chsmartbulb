// The service's socket protocol over a WebSocket: every message is one JSON object.
// Requests carry an `id` that the reply repeats; `{"event": "state"}` messages
// arrive on their own whenever anything changes the light.

/** One colour of a custom effect: blend into it over `fade` seconds, shaped by `ease`, then hold it. */
export type Step = { color: string; hold: number; fade: number; ease: string }

export type Value = number | string | string[] | Step[] | null

/** Another machine feeding its sound or screen. `name` and `monitors` are empty for an agent that never said who it is. */
export type AgentInfo = {
  id: string
  name: string | null
  monitors: { index: number; width: number; height: number }[]
  monitor: number | null
}

export type State = {
  connected: boolean
  link: 'connected' | 'connecting' | 'waiting'
  problem: string | null // why the service could not reach the bulb, while it cannot
  playing: boolean
  away?: 'lock' | 'sleep' | 'shutdown' | null // the computer is away: the bulb shows the away look, the plan waits
  on: boolean
  color: string
  brightness: number
  effect: { name: string; params: Record<string, Value> } | null
  native: { name: string; speed: number } | null
  audio: 'agent' | 'local'
  screen: 'agent' | 'local'
  agents: { audio: number; screen: number } // other machines feeding their sound or screen
  agentInfo?: { audio: AgentInfo[]; screen: AgentInfo[] } // who they are, from a service that knows
  localMonitors?: { index: number; width: number; height: number }[] // the service's own screens, when it can capture
  localMonitor?: number
  localScreenAsks?: boolean // the service's desktop has its user choose the screen (Wayland); `monitor` makes it ask again
  watchers: number // clients following the state, this page included
}

export type ParamSchema =
  | { type: 'number'; min: number; max: number; step: number }
  | { type: 'color'; optional: boolean }
  | { type: 'colors' }
  | { type: 'steps'; most: number; easings: string[] }

export type EffectInfo = {
  name: string
  summary: string
  params: Record<string, Value>
  schema: Record<string, ParamSchema>
  needs: 'audio' | 'screen' | null
}

export type Reply = { ok: boolean; error?: string; [key: string]: unknown }

export type Handlers = {
  online(state: State): void
  state(state: State): void
  offline(): void
  denied(): void
}

/** What the page talks to the service through: a WebSocket, or the app's IPC. */
export interface Link {
  request(message: Record<string, unknown>): Promise<Reply>
  /** Try again now instead of waiting out the retry delay, e.g. when the page is shown again. */
  hurry(): void
  close(): void
}

const FIRST_RETRY = 500
const SLOWEST_RETRY = 8000

export class Connection implements Link {
  #socket: WebSocket | null = null
  #pending = new Map<number, (reply: Reply) => void>()
  #nextId = 1
  #retry = FIRST_RETRY
  #timer: ReturnType<typeof setTimeout> | undefined
  #closed = false

  constructor(
    private url: string,
    private token: string,
    private handlers: Handlers,
  ) {
    this.#open()
  }

  #open(): void {
    const socket = new WebSocket(this.url)
    this.#socket = socket
    socket.onopen = async () => {
      // without a token, just ask: a service that wants none answers, any other says so
      const auth = this.token ? await this.request({ cmd: 'auth', token: this.token }) : { ok: true }
      const first = auth.ok ? await this.request({ cmd: 'subscribe' }) : auth
      if (first.ok) {
        this.#retry = FIRST_RETRY
        this.handlers.online(first as unknown as State)
      } else if (first.error === 'wrong token' || first.error === 'not authorised') {
        this.close()
        this.handlers.denied()
      }
    }
    socket.onmessage = (event) => {
      const message = JSON.parse(String(event.data))
      if (message.event === 'state') {
        this.handlers.state(message)
        return
      }
      const resolve = this.#pending.get(message.id)
      this.#pending.delete(message.id)
      resolve?.(message)
    }
    socket.onclose = () => {
      if (this.#socket !== socket) return
      for (const resolve of this.#pending.values()) resolve({ ok: false, error: 'connection lost' })
      this.#pending.clear()
      if (this.#closed) return
      this.handlers.offline()
      this.#timer = setTimeout(() => this.#open(), this.#retry)
      this.#retry = Math.min(this.#retry * 2, SLOWEST_RETRY)
    }
  }

  request(message: Record<string, unknown>): Promise<Reply> {
    const socket = this.#socket
    if (!socket || socket.readyState !== WebSocket.OPEN) {
      return Promise.resolve({ ok: false, error: 'not connected' })
    }
    const id = this.#nextId++
    return new Promise((resolve) => {
      this.#pending.set(id, resolve)
      socket.send(JSON.stringify({ ...message, id }))
    })
  }

  hurry(): void {
    if (this.#closed || this.#socket?.readyState !== WebSocket.CLOSED) return
    clearTimeout(this.#timer)
    this.#retry = FIRST_RETRY
    this.#open()
  }

  close(): void {
    this.#closed = true
    clearTimeout(this.#timer)
    this.#socket?.close()
  }
}
