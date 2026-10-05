// The service's socket protocol over a WebSocket: every message is one JSON object.
// Requests carry an `id` that the reply repeats; `{"event": "state"}` messages
// arrive on their own whenever anything changes the light.

export type Value = number | string | string[] | null

export type State = {
  connected: boolean
  link: 'connected' | 'connecting' | 'waiting'
  problem: string | null // why the service could not reach the bulb, while it cannot
  playing: boolean
  on: boolean
  color: string
  brightness: number
  effect: { name: string; params: Record<string, Value> } | null
  native: { name: string; speed: number } | null
  audio: string
  screen: string
}

export type ParamSchema =
  | { type: 'number'; min: number; max: number; step: number }
  | { type: 'color'; optional: boolean }
  | { type: 'colors' }

export type EffectInfo = {
  name: string
  summary: string
  params: Record<string, Value>
  schema: Record<string, ParamSchema>
  needs: 'audio' | 'screen' | null
}

export type Reply = { ok: boolean; error?: string; [key: string]: unknown }

type Handlers = {
  online(state: State): void
  state(state: State): void
  offline(): void
  denied(): void
}

const FIRST_RETRY = 500
const SLOWEST_RETRY = 8000

export class Connection {
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
      const auth = await this.request({ cmd: 'auth', token: this.token })
      if (!auth.ok) {
        if (auth.error === 'wrong token') {
          this.close()
          this.handlers.denied()
        }
        return
      }
      const first = await this.request({ cmd: 'subscribe' })
      if (first.ok) {
        this.#retry = FIRST_RETRY
        this.handlers.online(first as unknown as State)
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

  /** Try again now instead of waiting out the retry delay, e.g. when the page is shown again. */
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
