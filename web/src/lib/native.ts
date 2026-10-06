// The app's own service, reached over Tauri's IPC instead of a WebSocket. Only the
// app build loads this module; the page the Python service serves never does.

import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import type { Handlers, Link, Reply, State } from './api'

/** BLE needs the bulb free of a Classic link; SPP works while it is also the speaker. */
export type Bearer = 'ble' | 'spp'
export type Device = { address: string; name: string | null; bearer: Bearer }
export type Found = {
  address: string
  name: string | null
  rssi?: number | null
  bonded: boolean
  classic: boolean
  le: boolean
  likely: boolean
}
export type AudioInput = 'playback' | 'microphone'
export type Setup = { device: Device | null; audioInput: AudioInput; platform: string }
export type AudioRoute = { outputs: string[]; speakers: string[]; canCapturePlayback: boolean }
export type ShareStatus = {
  enabled: boolean
  token: string | null
  addresses: string[]
  linesPort: number
  webPort: number
  problem: string | null
}
/** Stands for the simulated bulb, for trying the app without one. */
export const SIMULATED = 'SIMULATED'

export const native = {
  setup: () => invoke<Setup>('setup'),
  prepare: () => invoke<{ granted: boolean; enabled: boolean }>('prepare'),
  scan: (seconds = 4) => invoke<Found[]>('scan', { seconds }),
  useDevice: (device: Device) => invoke<void>('use_device', { device }),
  forgetDevice: () => invoke<void>('forget_device'),
  setAudioInput: (input: AudioInput) => invoke<void>('set_audio_input', { input }),
  audioRoute: () => invoke<AudioRoute>('audio_route'),
  openSettings: (which: 'bluetooth' | 'sound') => invoke<void>('open_settings', { which }),
  shareStatus: () => invoke<ShareStatus>('share_status'),
  setSharing: (enabled: boolean, renew = false) => invoke<ShareStatus>('set_sharing', { enabled, renew }),
}

export class NativeLink implements Link {
  #closed = false
  #unlisten: Promise<UnlistenFn>

  constructor(private handlers: Handlers) {
    this.#unlisten = listen<State>('bulb-state', (event) => {
      if (!this.#closed) this.handlers.state(event.payload)
    })
    void this.#open()
  }

  async #open(): Promise<void> {
    const first = await this.request({ cmd: 'subscribe' })
    if (this.#closed) return
    if (first.ok) this.handlers.online(first as unknown as State)
    else this.handlers.offline()
  }

  async request(message: Record<string, unknown>): Promise<Reply> {
    try {
      return await invoke<Reply>('request', { message })
    } catch (error) {
      return { ok: false, error: String(error) }
    }
  }

  hurry(): void {
    // nothing to reconnect: the service runs in this process
  }

  close(): void {
    this.#closed = true
    void this.#unlisten.then((stop) => stop())
  }
}
