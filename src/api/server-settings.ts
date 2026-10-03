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

/** Proxy that broker calls leave through, so orders come from one fixed IP. */
export interface BrokerProxy {
  url: string
  username: string
  has_password: boolean
}

export interface BrokerProxyResponse {
  status: 'success' | 'error'
  message?: string
  data?: BrokerProxy
}

export const brokerProxyApi = {
  async get(): Promise<BrokerProxy> {
    const response = await webClient.get<BrokerProxyResponse>('/settings/api/broker-proxy')
    if (response.data.status !== 'success' || !response.data.data) {
      throw new Error(response.data.message || 'Could not load the proxy settings.')
    }
    return response.data.data
  },

  /** `password` omitted keeps the stored one; an empty string removes it. */
  async save(body: {
    url: string
    username: string
    password?: string
  }): Promise<BrokerProxyResponse> {
    const response = await webClient.post<BrokerProxyResponse>('/settings/api/broker-proxy', body)
    return response.data
  },
}
