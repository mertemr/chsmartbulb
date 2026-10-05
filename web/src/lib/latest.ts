/**
 * Wraps `send` for values that arrive faster than they can be delivered, as from a
 * dragged slider: one is in flight at a time and only the newest waits behind it.
 * Nothing queues up, so a slow device sets the pace by how fast it answers.
 */
export function latest<T>(send: (value: T) => Promise<unknown>): (value: T) => void {
  let busy = false
  let waiting: { value: T } | null = null

  async function run(value: T): Promise<void> {
    busy = true
    try {
      await send(value)
    } catch {
      // the caller reports failures; the next value must still go out
    }
    const next = waiting
    waiting = null
    if (next) void run(next.value)
    else busy = false
  }

  return (value) => {
    if (busy) waiting = { value }
    else void run(value)
  }
}
