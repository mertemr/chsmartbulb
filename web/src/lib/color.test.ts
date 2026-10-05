import { expect, test } from 'vitest'
import { cssColor, hueOf, mixed, parseHex, pure, rgbOnly, toHex, unmixed } from './color'

test('hex colours round-trip, with the white channel only when lit', () => {
  expect(parseHex('#ff0064')).toEqual({ r: 255, g: 0, b: 100, w: 0 })
  expect(parseHex('#000000ff')).toEqual({ r: 0, g: 0, b: 0, w: 255 })
  expect(toHex({ r: 255, g: 0, b: 100, w: 0 })).toBe('#ff0064')
  expect(toHex({ r: 0, g: 0, b: 0, w: 255 })).toBe('#000000ff')
  expect(toHex({ r: 300, g: -4, b: 0.4, w: 0 })).toBe('#ff0000')
})

test('every hue converts to a pure colour and back', () => {
  for (const hue of [0, 30, 60, 120, 180, 210, 240, 300, 359]) {
    expect(hueOf(pure(hue))).toBe(hue)
  }
  expect(toHex(pure(120))).toBe('#00ff00')
  expect(toHex(pure(360))).toBe('#ff0000')
  expect(hueOf({ r: 9, g: 9, b: 9, w: 0 })).toBeNull()
})

test('the white slider trades the hue for the white LEDs', () => {
  expect(mixed(0, 0)).toBe('#ff0000')
  expect(mixed(0, 1)).toBe('#000000ff')
  expect(mixed(240, 0.5)).toBe('#00008080')
  expect(unmixed('#00008080')).toEqual({ hue: 240, white: 0.5 })
  expect(unmixed('#000000ff')).toEqual({ hue: null, white: 1 })
  expect(unmixed('#102030')).toEqual({ hue: 210, white: 0 })
  expect(unmixed('#000000')).toEqual({ hue: null, white: 0 })
})

test('colours are painted at full strength, washed out by the white LEDs', () => {
  expect(cssColor('#800000')).toBe('rgb(255 0 0)')
  expect(cssColor('#000000ff')).toBe('rgb(255 255 255)')
  expect(cssColor('#00008080')).toBe('rgb(128 128 255)')
  expect(cssColor('#000000')).toBe('rgb(0 0 0)')
  expect(rgbOnly('#11223344')).toBe('#112233')
})
