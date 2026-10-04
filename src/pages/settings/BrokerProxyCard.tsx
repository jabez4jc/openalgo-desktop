/**
 * Desktop-only: the proxy broker calls go through.
 *
 * Brokers accept orders only from an IP address registered with them. On a
 * connection whose address changes, point OpenAlgo Desktop at a proxy on a
 * server with a fixed address and register that address with the broker.
 */

import { Loader2, Network, RefreshCw, Save } from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'
import { brokerProxyApi } from '@/api/server-settings'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { showToast } from '@/utils/toast'

interface FormState {
  url: string
  username: string
  password: string
}

export default function BrokerProxyCard() {
  const [saved, setSaved] = useState<FormState | null>(null)
  const [form, setForm] = useState<FormState | null>(null)
  const [hasPassword, setHasPassword] = useState(false)
  const [active, setActive] = useState(false)
  const [isLoading, setIsLoading] = useState(true)
  const [isSaving, setIsSaving] = useState(false)
  const [fetchError, setFetchError] = useState(false)

  const fetchCurrent = useCallback(async () => {
    setIsLoading(true)
    setFetchError(false)
    try {
      const p = await brokerProxyApi.get()
      const current = { url: p.url, username: p.username, password: '' }
      setSaved(current)
      setForm(current)
      setHasPassword(p.has_password)
      setActive(p.active)
    } catch {
      setFetchError(true)
      showToast.error('Could not load the proxy settings. Try again.')
    } finally {
      setIsLoading(false)
    }
  }, [])

  useEffect(() => {
    fetchCurrent()
  }, [fetchCurrent])

  const update = (patch: Partial<FormState>) => setForm((f) => (f ? { ...f, ...patch } : f))

  const handleSave = async () => {
    if (!form) return
    setIsSaving(true)
    try {
      const res = await brokerProxyApi.save({
        url: form.url.trim(),
        username: form.username.trim(),
        // Left blank, the stored password is kept.
        ...(form.password ? { password: form.password } : {}),
      })
      if (res.status === 'success') {
        const p = res.data
        const next = { url: p?.url ?? '', username: p?.username ?? '', password: '' }
        setSaved(next)
        setForm(next)
        setHasPassword(p?.has_password ?? false)
        setActive(p?.active ?? false)
        showToast.success(res.message || 'Proxy settings saved.')
      } else {
        showToast.error(res.message || 'The proxy settings were not saved. Try again.')
      }
    } catch (error) {
      const message = error instanceof Error && error.message ? error.message : ''
      showToast.error(message || 'The proxy settings were not saved. Try again.')
    } finally {
      setIsSaving(false)
    }
  }

  const removePassword = async () => {
    if (!form) return
    setIsSaving(true)
    try {
      const res = await brokerProxyApi.save({
        url: form.url.trim(),
        username: form.username.trim(),
        password: '',
      })
      if (res.status === 'success') {
        setHasPassword(res.data?.has_password ?? false)
        showToast.success(res.message || 'Proxy password removed.')
      } else {
        showToast.error(res.message || 'The proxy password was not removed. Try again.')
      }
    } catch {
      showToast.error('The proxy password was not removed. Try again.')
    } finally {
      setIsSaving(false)
    }
  }

  const isModified =
    form !== null && saved !== null && JSON.stringify(form) !== JSON.stringify(saved)

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center gap-3">
          <Network className="h-6 w-6 text-primary" />
          <div>
            <CardTitle>Broker Proxy</CardTitle>
            <CardDescription>
              Send broker calls through a proxy on a server with a fixed IP address, then register
              that address with your broker. Leave the address empty to connect directly. Changes
              apply after OpenAlgo restarts.
            </CardDescription>
          </div>
        </div>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
        ) : fetchError || !form ? (
          <div className="space-y-4">
            <p className="text-sm text-muted-foreground">The proxy settings could not be loaded.</p>
            <Button size="sm" variant="destructive" onClick={fetchCurrent}>
              <RefreshCw className="h-4 w-4 mr-1" />
              Retry
            </Button>
          </div>
        ) : (
          <form
            className="space-y-4"
            onSubmit={(e) => {
              e.preventDefault()
              handleSave()
            }}
          >
            <p className="text-sm">
              {active
                ? 'In use: broker calls are going through the proxy.'
                : saved?.url
                  ? 'Saved but not in use yet. Restart OpenAlgo to start using it.'
                  : 'Not in use: broker calls connect directly.'}
            </p>
            <div className="space-y-2">
              <Label htmlFor="proxy-url">Proxy address</Label>
              <Input
                id="proxy-url"
                placeholder="http://203.0.113.10:3128"
                value={form.url}
                onChange={(e) => update({ url: e.target.value })}
                autoComplete="off"
              />
            </div>
            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-2">
                <Label htmlFor="proxy-user">Username</Label>
                <Input
                  id="proxy-user"
                  value={form.username}
                  onChange={(e) => update({ username: e.target.value })}
                  autoComplete="off"
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="proxy-pass">Password</Label>
                <Input
                  id="proxy-pass"
                  type="password"
                  placeholder={hasPassword ? 'Saved. Type to replace.' : ''}
                  value={form.password}
                  onChange={(e) => update({ password: e.target.value })}
                  autoComplete="new-password"
                />
              </div>
            </div>
            <div className="flex items-center gap-2">
              <Button type="submit" disabled={isSaving || !isModified}>
                {isSaving ? (
                  <Loader2 className="h-4 w-4 animate-spin mr-1" />
                ) : (
                  <Save className="h-4 w-4 mr-1" />
                )}
                Save
              </Button>
              {hasPassword && (
                <Button
                  type="button"
                  variant="outline"
                  disabled={isSaving}
                  onClick={removePassword}
                >
                  Remove saved password
                </Button>
              )}
            </div>
          </form>
        )}
      </CardContent>
    </Card>
  )
}
