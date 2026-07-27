/**
 * Test-only readers for the Rust source of truth.
 *
 * The editor duplicates a handful of Rust constants — parameter ids, ranges,
 * and the tank geometry the decay display draws. Duplication is unavoidable
 * (the editor is a separate bundle), so the tests parse the real `.rs` files
 * and compare, which turns a silent drift into a failing check.
 *
 * Not imported by the app; kept out of the bundle by having no runtime caller.
 */

import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const read = (relative: string) =>
  readFileSync(fileURLToPath(new URL(relative, import.meta.url)), 'utf8')

export const LIB_RS = read('../../src/lib.rs')
export const IPC_RS = read('../../src/ipc.rs')

/** Rust float literals allow `_` separators; JS `Number` does not. */
function num(literal: string): number {
  return Number(literal.replace(/_/g, ''))
}

/** The body of `<name>: <type> = [ ... ];`. */
function arrayBody(source: string, name: string): string {
  const match = new RegExp(
    `(?:pub )?const ${name}\\s*:[^=]+=\\s*\\[([\\s\\S]*?)\\];`,
  ).exec(source)
  if (!match?.[1]) throw new Error(`${name} not found in Rust source`)
  return match[1]
}

/** `pub const NAME: f32 = 500.0;` */
export function rustScalar(source: string, name: string): number {
  const match = new RegExp(
    `(?:pub )?const ${name}\\s*:\\s*f32\\s*=\\s*([0-9_.]+)\\s*;`,
  ).exec(source)
  if (!match?.[1]) throw new Error(`${name} not found in Rust source`)
  return num(match[1])
}

export function rustFloatArray(source: string, name: string): number[] {
  return arrayBody(source, name)
    .split(',')
    .map((part) => part.trim())
    .filter((part) => part.length > 0)
    .map(num)
}

/** `UI_PARAM_IDS` — the quoted strings, in order. */
export function rustStringArray(source: string, name: string): string[] {
  return Array.from(arrayBody(source, name).matchAll(/"([^"]*)"/g), (m) => m[1]!)
}

/**
 * `RANGES` — `(min, max), // comment` tuples, in order. Identifiers are
 * resolved against `lib.rs` scalars so `MAX_PREDELAY_MS` reads as 500.
 */
export function rustRanges(name: string): [number, number][] {
  const resolve = (token: string) =>
    /^[-0-9]/.test(token) ? num(token) : rustScalar(LIB_RS, token)
  return Array.from(
    arrayBody(IPC_RS, name).matchAll(/\(\s*([^,\s]+)\s*,\s*([^)\s]+)\s*\)/g),
    (m) => [resolve(m[1]!), resolve(m[2]!)] as [number, number],
  )
}

/** Variant order of `enum Mode`, which is the `to_wire` contract. */
export function rustModeOrder(): string[] {
  const match = /pub enum Mode \{([\s\S]*?)\n\}/.exec(LIB_RS)
  if (!match?.[1]) throw new Error('enum Mode not found')
  return Array.from(match[1].matchAll(/^\s{4}(\w+),/gm), (m) =>
    m[1]!.toLowerCase(),
  )
}
