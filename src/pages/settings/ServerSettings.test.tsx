import { beforeEach, describe, expect, it, vi } from 'vitest'
import { axe } from '@/test/a11y-utils'
import { render, screen, userEvent, waitFor } from '@/test/test-utils'

const api = vi.hoisted(() => ({ get: vi.fn(), save: vi.fn() }))
const toast = vi.hoisted(() => ({ success: vi.fn(), error: vi.fn() }))

vi.mock('@/api/server-settings', () => ({ serverSettingsApi: api }))
vi.mock('@/utils/toast', () => ({ showToast: toast }))

import ServerSettings, { validateServerSettings } from './ServerSettings'

const LOOPBACK = {
  http_host: '127.0.0.1',
  http_port: 5000,
  ws_host: '127.0.0.1',
  ws_port: 8765,
  lan_enabled: false,
}

beforeEach(() => {
  api.get.mockReset().mockResolvedValue(LOOPBACK)
  api.save.mockReset()
  toast.success.mockReset()
  toast.error.mockReset()
})

describe('validateServerSettings', () => {
  const ok = {
    http_host: '127.0.0.1',
    http_port: '5000',
    ws_host: '127.0.0.1',
    ws_port: '8765',
    lan_enabled: false,
  }

  it('accepts the shipped defaults', () => {
    expect(validateServerSettings(ok)).toBeNull()
  })

  it('names the field a trader got wrong', () => {
    expect(validateServerSettings({ ...ok, http_port: '80' })).toMatch(/app port/)
    expect(validateServerSettings({ ...ok, ws_port: 'abc' })).toMatch(/market data port/)
    expect(validateServerSettings({ ...ok, ws_port: '5000' })).toMatch(/must be different/)
    expect(validateServerSettings({ ...ok, ws_host: ' ' })).toMatch(/address/)
  })
})

describe('Server Settings page', () => {
  it('loads the current addresses', async () => {
    render(<ServerSettings />)
    expect(await screen.findByLabelText('App port')).toHaveValue('5000')
    expect(screen.getByLabelText('Market data port')).toHaveValue('8765')
    expect(screen.getByRole('button', { name: /save/i })).toBeDisabled()
  })

  it('switches loopback hosts to all interfaces when LAN access is turned on, and saves', async () => {
    const user = userEvent.setup()
    api.save.mockResolvedValue({ status: 'success', message: 'Saved.' })
    render(<ServerSettings />)
    await user.click(await screen.findByRole('switch', { name: /other devices/i }))
    expect(screen.getByLabelText('App address')).toHaveValue('0.0.0.0')
    expect(screen.getByText(/anyone on your network/i)).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /save/i }))
    await waitFor(() => expect(api.save).toHaveBeenCalledTimes(1))
    expect(api.save).toHaveBeenCalledWith({
      http_host: '0.0.0.0',
      http_port: 5000,
      ws_host: '0.0.0.0',
      ws_port: 8765,
      lan_enabled: true,
    })
    expect(toast.success).toHaveBeenCalledWith('Saved.')
  })

  it('warns about broker redirect URLs when the app port changes', async () => {
    const user = userEvent.setup()
    render(<ServerSettings />)
    const port = await screen.findByLabelText('App port')
    await user.clear(port)
    await user.type(port, '5500')
    expect(screen.getByText(/update your broker app/i)).toBeInTheDocument()
  })

  it('shows the server message when a port is refused', async () => {
    const user = userEvent.setup()
    api.save.mockResolvedValue({
      status: 'error',
      message: 'Port 5500 is already in use by another app. Choose a different port.',
    })
    render(<ServerSettings />)
    const port = await screen.findByLabelText('App port')
    await user.clear(port)
    await user.type(port, '5500')
    await user.click(screen.getByRole('button', { name: /save/i }))
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith(
        'Port 5500 is already in use by another app. Choose a different port.'
      )
    )
  })

  it('offers a retry when the settings cannot be loaded', async () => {
    api.get.mockRejectedValueOnce(new Error('offline'))
    const user = userEvent.setup()
    render(<ServerSettings />)
    await user.click(await screen.findByRole('button', { name: /retry/i }))
    expect(await screen.findByLabelText('App port')).toHaveValue('5000')
  })

  it('has no accessibility violations', async () => {
    const { container } = render(<ServerSettings />)
    await screen.findByLabelText('App port')
    expect(await axe(container)).toHaveNoViolations()
  })
})
