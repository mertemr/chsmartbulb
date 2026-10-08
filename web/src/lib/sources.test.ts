import { expect, test } from 'vitest'
import type { State } from './api'
import { feedOf, feedText } from './sources'

const base = { agents: { audio: 0, screen: 0 }, agentInfo: { audio: [], screen: [] } } as unknown as State
const areas = [
  { index: 1, width: 1920, height: 1080 },
  { index: 2, width: 1280, height: 1024 },
]

test('the machine itself feeds the light when no agent does', () => {
  expect(feedOf(base, 'audio', true)).toEqual({ agent: false, from: 'this device', monitor: null })
  expect(feedText('audio', feedOf(base, 'audio', false))).toBe('Sound: the service’s computer')
  expect(feedOf(null, 'screen', false).agent).toBe(false)
  // the app has no screen of its own to follow
  expect(feedText('screen', feedOf(base, 'screen', true))).toBe('Screen: no computer yet')
})

test('an agent is named, and its monitor shown only when it has a choice', () => {
  const state = {
    ...base,
    agentInfo: { audio: [], screen: [{ id: 'screen-1', name: 'MERT-PC', monitors: areas, monitor: 2 }] },
  } as State
  expect(feedText('screen', feedOf(state, 'screen', false))).toBe('Screen: MERT-PC (agent) · monitor 2')

  const single = {
    ...base,
    agentInfo: { audio: [], screen: [{ id: 'screen-1', name: 'MERT-PC', monitors: areas.slice(0, 1), monitor: 1 }] },
  } as State
  expect(feedText('screen', feedOf(single, 'screen', false))).toBe('Screen: MERT-PC (agent)')

  const nameless = { ...base, agentInfo: { audio: [{ id: 'audio-1', name: null, monitors: [], monitor: null }], screen: [] } } as State
  expect(feedText('audio', feedOf(nameless, 'audio', false))).toBe('Sound: another computer (agent)')
})

test('the services own screen shows the monitor it follows', () => {
  const state = { ...base, localMonitors: areas, localMonitor: 0 } as State
  expect(feedText('screen', feedOf(state, 'screen', false))).toBe('Screen: the service’s computer · all monitors')
})
