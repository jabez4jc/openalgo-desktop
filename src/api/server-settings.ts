/**
 * Desktop-only: the local server's listen addresses.
 *
 * OpenAlgo web reads these from .env; the desktop app has no .env, so they are
 * read and changed in-app through the local server's settings endpoint.
 */

import { webClient } from './client'

export interface ServerSettings {
  http_host: string
  http_port: number
  ws_host: string
  ws_port: number
  lan_enabled: boolean
}

export interface ServerSettingsResponse {
  status: 'success' | 'error'
  message?: string
  data?: ServerSettings
}

export const serverSettingsApi = {
  async get(): Promise<ServerSettings> {
    const response = await webClient.get<ServerSettingsResponse>('/settings/api/server')
    if (response.data.status !== 'success' || !response.data.data) {
      throw new Error(response.data.message || 'Could not load the server settings.')
    }
    return response.data.data
  },

  async save(settings: ServerSettings): Promise<ServerSettingsResponse> {
    const response = await webClient.post<ServerSettingsResponse>('/settings/api/server', settings)
    return response.data
  },
}
