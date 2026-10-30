import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test'

const originalCore = { ...(await import('@tauri-apps/api/core')) }
const calls: Array<{ command: string; args?: unknown }> = []
let stored = true
let failing = false
mock.module('@tauri-apps/api/core', () => ({
  ...originalCore,
  invoke: async (command: string, args?: unknown) => {
    calls.push({ command, args })
    if (failing) throw new Error('backend unavailable')
    return command.startsWith('get_') ? stored : undefined
  },
}))

const { cachedBackendFlag, readBackendFlag, writeBackendFlag } = await import('./backendFlag')

const originalWindow = globalThis.window
const originalLocalStorage = globalThis.localStorage
const storage = new Map<string, string>()
const events: unknown[] = []

beforeEach(() => {
  calls.length = 0
  events.length = 0
  storage.clear()
  stored = true
  failing = false
  const target = new EventTarget()
  target.addEventListener('flag-changed', (event) => events.push((event as CustomEvent).detail))
  Object.defineProperty(globalThis, 'window', { configurable: true, value: target })
  Object.defineProperty(globalThis, 'localStorage', {
    configurable: true,
    value: { getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, value: string) => storage.set(key, value) },
  })
})

afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore)
  if (originalWindow) Object.defineProperty(globalThis, 'window', { configurable: true, value: originalWindow })
  else Reflect.deleteProperty(globalThis, 'window')
  if (originalLocalStorage) Object.defineProperty(globalThis, 'localStorage', { configurable: true, value: originalLocalStorage })
  else Reflect.deleteProperty(globalThis, 'localStorage')
})

describe('backend flags', () => {
  test('a read mirrors the backend into the cache and falls back to it', async () => {
    expect(await readBackendFlag('get_flag', { cacheKey: 'flag' })).toBe(true)
    expect(cachedBackendFlag({ cacheKey: 'flag' })).toBe(true)
    failing = true
    stored = false
    expect(await readBackendFlag('get_flag', { cacheKey: 'flag' })).toBe(true)
    expect(await readBackendFlag('get_flag')).toBe(false)
  })

  test('a cached flag is announced before it is saved', async () => {
    failing = true
    await expect(writeBackendFlag('set_flag', 'flag-changed', true, { cacheKey: 'flag' })).rejects.toThrow()
    expect(events).toEqual([true])
    expect(storage.get('flag')).toBe('true')
  })

  test('an uncached flag is announced only once the backend accepts it', async () => {
    await writeBackendFlag('set_flag', 'flag-changed', true)
    expect(calls).toEqual([{ command: 'set_flag', args: { enabled: true } }])
    expect(events).toEqual([true])
    failing = true
    await expect(writeBackendFlag('set_flag', 'flag-changed', false)).rejects.toThrow()
    expect(events).toEqual([true])
  })
})
