import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const tauri = vi.hoisted(() => ({ inShell: false, open: vi.fn(async (_url: string) => {}) }))

vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => tauri.inShell }))
vi.mock('@tauri-apps/plugin-shell', () => ({ open: tauri.open }))

import {
  DEFAULT_WEBSOCKET_URL,
  desktopProfileMenuItems,
  installDesktopShellHandlers,
  isDesktopShell,
  isExternalHttpUrl,
} from './desktop'

const flush = () => new Promise((resolve) => setTimeout(resolve, 0))

function clickLink(href: string, attrs: Record<string, string> = {}): MouseEvent {
  const link = document.createElement('a')
  link.setAttribute('href', href)
  for (const [k, v] of Object.entries(attrs)) link.setAttribute(k, v)
  link.textContent = 'link'
  document.body.appendChild(link)
  const event = new MouseEvent('click', { bubbles: true, cancelable: true, button: 0 })
  link.dispatchEvent(event)
  link.remove()
  return event
}

describe('isExternalHttpUrl', () => {
  const base = 'http://127.0.0.1:5000/dashboard'

  it('treats another origin over http(s) as external', () => {
    expect(isExternalHttpUrl('https://docs.openalgo.in', base)).toBe(true)
    expect(isExternalHttpUrl('http://127.0.0.1:5001/x', base)).toBe(true)
  })

  it('keeps the app origin, relative paths and non-web schemes inside the app', () => {
    expect(isExternalHttpUrl('/orderbook', base)).toBe(false)
    expect(isExternalHttpUrl('http://127.0.0.1:5000/positions', base)).toBe(false)
    expect(isExternalHttpUrl('mailto:support@openalgo.in', base)).toBe(false)
    expect(isExternalHttpUrl('blob:http://127.0.0.1:5000/abc', base)).toBe(false)
    expect(isExternalHttpUrl('javascript:void(0)', base)).toBe(false)
  })
})

describe('desktop constants', () => {
  it('falls back to the development feed port under vitest', () => {
    expect(DEFAULT_WEBSOCKET_URL).toBe('ws://127.0.0.1:8766')
  })

  it('adds Server Settings to the profile menu', () => {
    expect(desktopProfileMenuItems.map((i) => i.href)).toEqual(['/settings/server'])
  })
})

describe('installDesktopShellHandlers', () => {
  let uninstall: () => void = () => {}
  const originalOpen = window.open

  beforeEach(() => {
    tauri.open.mockClear()
  })

  afterEach(() => {
    uninstall()
    tauri.inShell = false
    window.open = originalOpen
  })

  it('does nothing in a plain browser', () => {
    tauri.inShell = false
    expect(isDesktopShell()).toBe(false)
    uninstall = installDesktopShellHandlers()
    expect(window.open).toBe(originalOpen)
    const event = clickLink('https://docs.openalgo.in', { target: '_blank' })
    expect(event.defaultPrevented).toBe(false)
    expect(tauri.open).not.toHaveBeenCalled()
  })

  it('sends external links to the system browser inside the shell', async () => {
    tauri.inShell = true
    uninstall = installDesktopShellHandlers()
    const event = clickLink('https://docs.openalgo.in/', { target: '_blank' })
    await flush()
    expect(event.defaultPrevented).toBe(true)
    expect(tauri.open).toHaveBeenCalledWith('https://docs.openalgo.in/')
  })

  it('leaves in-app links to the router', async () => {
    tauri.inShell = true
    uninstall = installDesktopShellHandlers()
    const event = clickLink('/orderbook')
    await flush()
    expect(event.defaultPrevented).toBe(false)
    expect(tauri.open).not.toHaveBeenCalled()
  })

  it('respects a click a component already handled', async () => {
    tauri.inShell = true
    uninstall = installDesktopShellHandlers()
    const link = document.createElement('a')
    link.href = 'https://docs.openalgo.in/'
    link.addEventListener('click', (e) => e.preventDefault())
    document.body.appendChild(link)
    link.click()
    link.remove()
    await flush()
    expect(tauri.open).not.toHaveBeenCalled()
  })

  it('routes window.open: external to the browser, same-origin exports to a download', async () => {
    tauri.inShell = true
    uninstall = installDesktopShellHandlers()
    expect(window.open('https://openalgo.in/discord', '_blank')).toBeNull()
    await flush()
    expect(tauri.open).toHaveBeenCalledWith('https://openalgo.in/discord')

    const clicks: string[] = []
    const spy = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (
      this: HTMLAnchorElement
    ) {
      clicks.push(this.getAttribute('href') ?? '')
    })
    expect(window.open('/traffic/export', '_blank')).toBeNull()
    spy.mockRestore()
    expect(clicks).toEqual(['/traffic/export'])
    expect(tauri.open).toHaveBeenCalledTimes(1)
  })

  it('restores window.open when removed', () => {
    tauri.inShell = true
    uninstall = installDesktopShellHandlers()
    expect(window.open).not.toBe(originalOpen)
    uninstall()
    uninstall = () => {}
    expect(window.open).toBe(originalOpen)
  })
})
