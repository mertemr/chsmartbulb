// The bulb has red, green and blue LEDs plus separate white ones. A colour on the
// wire is "#rrggbb", or "#rrggbbww" when the white LEDs are lit.

export type Rgbw = { r: number; g: number; b: number; w: number }

export function parseHex(hex: string): Rgbw {
  const digits = hex.replace('#', '')
  const byte = (i: number) => parseInt(digits.slice(i, i + 2) || '00', 16) || 0
  return { r: byte(0), g: byte(2), b: byte(4), w: byte(6) }
}

export function toHex({ r, g, b, w }: Rgbw): string {
  const pair = (value: number) => Math.max(0, Math.min(255, Math.round(value))).toString(16).padStart(2, '0')
  return `#${pair(r)}${pair(g)}${pair(b)}${w > 0 ? pair(w) : ''}`
}

/** The fully saturated colour at `hue` degrees. */
export function pure(hue: number): Rgbw {
  const h = (((hue % 360) + 360) % 360) / 60
  const x = 1 - Math.abs((h % 2) - 1)
  const [r, g, b] = [
    [1, x, 0],
    [x, 1, 0],
    [0, 1, x],
    [0, x, 1],
    [x, 0, 1],
    [1, 0, x],
  ][Math.floor(h) % 6] as [number, number, number]
  return { r: r * 255, g: g * 255, b: b * 255, w: 0 }
}

/** Hue in degrees of the red, green and blue part, or null when that part is grey or dark. */
export function hueOf({ r, g, b }: Rgbw): number | null {
  const high = Math.max(r, g, b)
  const span = high - Math.min(r, g, b)
  if (span === 0) return null
  const sector = high === r ? ((g - b) / span) % 6 : high === g ? (b - r) / span + 2 : (r - g) / span + 4
  return Math.round((sector * 60 + 360) % 360)
}

/**
 * A colour as two sliders: a hue, and how much of the light comes from the white
 * LEDs (0 = the pure hue, 1 = white only). Pastels are made this way because the
 * bulb's red, green and blue together give a blue-violet, not a white.
 */
export function mixed(hue: number, white: number): string {
  const share = Math.max(0, Math.min(1, white))
  const { r, g, b } = pure(hue)
  return toHex({ r: r * (1 - share), g: g * (1 - share), b: b * (1 - share), w: 255 * share })
}

/** The sliders for a colour; `hue` is null when only the white LEDs are lit. */
export function unmixed(hex: string): { hue: number | null; white: number } {
  const color = parseHex(hex)
  const tint = Math.max(color.r, color.g, color.b)
  return { hue: hueOf(color), white: color.w + tint === 0 ? 0 : color.w / (color.w + tint) }
}

/** Something to paint on the screen for a bulb colour: the white LEDs wash the tint out. */
export function cssColor(hex: string): string {
  const { r, g, b, w } = parseHex(hex)
  const top = Math.max(r, g, b) + w
  if (top === 0) return 'rgb(0 0 0)'
  const lift = (value: number) => Math.round(Math.min(255, ((value + w) / top) * 255))
  return `rgb(${lift(r)} ${lift(g)} ${lift(b)})`
}

/** What `<input type="color">` accepts: six digits, the white LEDs dropped. */
export function rgbOnly(hex: string): string {
  return toHex({ ...parseHex(hex), w: 0 })
}
