import type { SelectOption } from './select'
import { beaconOrigin } from './beacon-location'
import { clientPerformance } from './client-performance'

export interface BeaconInstallation {
  id: string
  slug?: string
  name: string
  role: 'owner' | 'member'
  online: boolean
  updateRequired: boolean
}

export interface AccountSession {
  authenticated: boolean
  account: { id: string, email: string } | null
  csrf: string | null
  installations: BeaconInstallation[]
}

export interface DeviceReview {
  code: string
  name: string
  fingerprint: string
  confirmation: string
}

export const INSTALLATION_INSTRUCTIONS = 'This code expires in 10 minutes. Run this command on your Linux x86-64 machine. No domain, certificate, incoming port or S3 setup is needed.'

export function formatInstallationCommand(claimCode: string) {
  const quote = (value: string) => `'${value.replaceAll('\'', '\'"\'"\'')}'`
  return `curl -fsSL ${quote(`${beaconOrigin}/install.sh`)} | sudo bash -s -- --claim-code ${quote(claimCode)}`
}

export function installationStatus(value: { online: boolean, updateRequired: boolean }) {
  if (value.updateRequired)
    return 'Mise à jour nécessaire'
  return value.online ? 'Online' : 'Offline'
}

export function buildInstallationOptions(installations: BeaconInstallation[]): SelectOption[] {
  return installations.map(item => ({
    value: item.id,
    label: item.name,
    description: item.role === 'owner' ? 'Owner' : 'Member',
    keywords: [installationStatus(item)],
    status: {
      label: installationStatus(item),
      tone: item.updateRequired ? 'warning' : item.online ? 'success' : 'muted',
    },
  }))
}

async function accountRequestOnce(route: string, csrf: string, body: unknown, scope: ReturnType<typeof clientPerformance.capture>) {
  const response = await fetch(`/api/account/${route}`, {
    method: body === undefined ? 'GET' : 'POST',
    credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  if (response.status === 204)
    return
  const text = await response.text()
  clientPerformance.probe('API')
  const value = text ? clientPerformance.measure('JSON_DECODE', () => JSON.parse(text), scope) : undefined
  if (!response.ok)
    throw new Error(value?.error || 'Unable to reach Cairn. Please try again.')
  return value
}

export async function beaconAccountRequest(route: string, csrf: string, body?: unknown) {
  const scope = clientPerformance.capture()
  const finish = clientPerformance.begin(body === undefined ? 'API_READ' : 'API_WRITE', scope)
  try {
    const value = await accountRequestOnce(route, csrf, body, scope)
    finish?.('ok')
    return value
  }
  catch (error) {
    finish?.('error')
    throw error
  }
}

export async function beaconInstallationRequest(route: string, csrf: string, body?: unknown, method: 'POST' | 'DELETE' = 'POST') {
  const response = await fetch(`/api/installations/${route}`, {
    method,
    credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  if (response.status === 204)
    return
  const value = await response.json()
  if (!response.ok)
    throw new Error(value.error || 'Unable to update the installation.')
  return value
}
