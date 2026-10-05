import { expect, test } from 'vitest'
import { latest } from './latest'

test('only the newest value waits behind the one in flight', async () => {
  const sent: number[] = []
  const release: (() => void)[] = []
  const push = latest(
    (value: number) =>
      new Promise<void>((resolve) => {
        sent.push(value)
        release.push(resolve)
      }),
  )
  const settle = () => new Promise((resolve) => setTimeout(resolve, 0))

  push(1)
  push(2)
  push(3)
  expect(sent).toEqual([1]) // 2 was dropped, 3 waits
  release[0]!()
  await settle()
  expect(sent).toEqual([1, 3])
  release[1]!()
  await settle()
  push(4)
  expect(sent).toEqual([1, 3, 4])
})

test('a failed send does not block the next one', async () => {
  const sent: number[] = []
  const push = latest(async (value: number) => {
    sent.push(value)
    throw new Error('down')
  })
  push(1)
  await new Promise((resolve) => setTimeout(resolve, 0))
  push(2)
  expect(sent).toEqual([1, 2])
})
