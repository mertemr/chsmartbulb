import type { State } from './api'

export type Kind = 'audio' | 'screen'

/** Who feeds the light with sound or screen, in words. */
export type Feed = {
  /** another computer's agent feeds it, as opposed to the machine the service runs on */
  agent: boolean
  /** the machine(s) it comes from */
  from: string
  /** the monitor being followed, when the machine has more than one; 0 is all of them */
  monitor: number | null
}

export function monitorName(index: number): string {
  return index === 0 ? 'all monitors' : `monitor ${index}`
}

/** `own`: the service is this app's own, so "the machine it runs on" is this device. */
export function feedOf(state: State | null, kind: Kind, own: boolean): Feed {
  const agents = state?.agentInfo?.[kind] ?? []
  if (agents.length) {
    const only = agents.length === 1 ? agents[0]! : null
    return {
      agent: true,
      from: agents.map((agent) => agent.name ?? 'another computer').join(', '),
      monitor: kind === 'screen' && only && only.monitors.length > 1 ? only.monitor : null,
    }
  }
  const monitors = state?.localMonitors ?? []
  return {
    agent: false,
    // the app follows no screen of its own; only an agent brings one
    from: own ? (kind === 'screen' ? 'no computer yet' : 'this device') : 'the service’s computer',
    monitor: kind === 'screen' && monitors.length > 1 ? (state?.localMonitor ?? null) : null,
  }
}

/** One line for the badge: "Screen: MERT-PC (agent) · monitor 2". */
export function feedText(kind: Kind, feed: Feed): string {
  const what = kind === 'audio' ? 'Sound' : 'Screen'
  const monitor = feed.monitor === null ? '' : ` · ${monitorName(feed.monitor)}`
  return `${what}: ${feed.from}${feed.agent ? ' (agent)' : ''}${monitor}`
}
