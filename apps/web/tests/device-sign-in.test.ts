import type { DeviceFlow } from '../src/device-sign-in'
import {
  afterEach,
  beforeEach,
  expect,
  it,
  vi,
} from 'vitest'
import { api, ApiError } from '../src/api'
import {
  credentialInput,
  signInProblem,
  useDeviceSignIn,
  verificationLink,
} from '../src/device-sign-in'

vi.mock('../src/api', () => ({
  api: vi.fn(),
  ApiError: class extends Error {
    constructor(message: string, readonly status: number) { super(message) }
  },
}))

const pending: DeviceFlow = {
  id: 'flow',
  pluginId: 'github',
  state: 'pending',
  userCode: 'ABCD-1234',
  verificationUri: 'https://github.com/login/device',
  expiresAt: 900_000,
}

beforeEach(() => {
  vi.mocked(api).mockReset()
  vi.useFakeTimers()
})
afterEach(() => vi.useRealTimers())

it('sends a completed sign-in instead of a pasted token, and nothing to keep the saved one', () => {
  expect(credentialInput('', null)).toEqual({})
  expect(credentialInput('ghp_pasted', null)).toEqual({ token: 'ghp_pasted' })
  expect(credentialInput('ghp_pasted', pending)).toEqual({ token: 'ghp_pasted' })
  expect(credentialInput('', { ...pending, state: 'completed' })).toEqual({ credentialFlow: 'flow' })
  expect(verificationLink(pending)).toBe('https://github.com/login/device')
  expect(verificationLink({ ...pending, verificationUriComplete: 'https://github.com/login/device?code=ABCD' })).toBe('https://github.com/login/device?code=ABCD')
  expect(signInProblem(pending)).toBe('')
  expect(signInProblem({ state: 'failed', message: 'Access denied' })).toBe('Access denied')
  expect(signInProblem({ state: 'expired' })).toBe('This sign-in expired. Sign in again.')
})

it('polls every 2 s until the sign-in completes, then hands it over once', async () => {
  const completed = vi.fn()
  const signIn = useDeviceSignIn(() => 'github', completed)
  vi.mocked(api)
    .mockResolvedValueOnce(pending)
    .mockResolvedValueOnce(pending)
    .mockResolvedValueOnce({ ...pending, state: 'completed' })

  await signIn.start()
  expect(vi.mocked(api).mock.calls[0]).toEqual(['/plugins/github/device-flow', { method: 'POST' }])
  expect(signIn.pending.value).toBe(true)

  await vi.advanceTimersByTimeAsync(1999)
  expect(api).toHaveBeenCalledTimes(1)
  await vi.advanceTimersByTimeAsync(1)
  expect(vi.mocked(api).mock.calls[1]).toEqual(['/plugins/github/device-flow/flow'])
  await vi.advanceTimersByTimeAsync(2000)
  expect(signIn.completed.value).toBe(true)
  expect(completed).toHaveBeenCalledWith(expect.objectContaining({ id: 'flow', state: 'completed' }))

  // Settled: no more polling, and a save consumes it without a cancellation.
  await vi.advanceTimersByTimeAsync(10_000)
  expect(api).toHaveBeenCalledTimes(3)
  signIn.consumed()
  expect(signIn.flow.value).toBeNull()
  expect(api).toHaveBeenCalledTimes(3)
})

it('cancels on the installation, and reads a forgotten sign-in as expired', async () => {
  const signIn = useDeviceSignIn(() => 'github', vi.fn())
  vi.mocked(api).mockResolvedValueOnce(pending).mockResolvedValueOnce({ cancelled: true })
  await signIn.start()
  await signIn.cancel()
  expect(vi.mocked(api).mock.calls[1]).toEqual(['/plugins/github/device-flow/flow', { method: 'DELETE' }])
  expect(signIn.flow.value).toBeNull()
  await vi.advanceTimersByTimeAsync(4000)
  expect(api).toHaveBeenCalledTimes(2)

  vi.mocked(api).mockResolvedValueOnce(pending).mockRejectedValueOnce(new ApiError('This sign-in expired. Sign in again.', 404))
  await signIn.start()
  await vi.advanceTimersByTimeAsync(2000)
  expect(signIn.flow.value?.state).toBe('expired')
  expect(signInProblem(signIn.flow.value)).toBe('This sign-in expired. Sign in again.')
  await vi.advanceTimersByTimeAsync(4000)
  expect(api).toHaveBeenCalledTimes(4)
})

it('keeps polling through a transient error, and reports a failed start', async () => {
  const signIn = useDeviceSignIn(() => 'github', vi.fn())
  vi.mocked(api)
    .mockResolvedValueOnce(pending)
    .mockRejectedValueOnce(new ApiError('Unavailable', 503))
    .mockResolvedValueOnce({ ...pending, state: 'failed', message: 'Access denied' })
  await signIn.start()
  await vi.advanceTimersByTimeAsync(2000)
  expect(signIn.error.value).toBe('Unavailable')
  await vi.advanceTimersByTimeAsync(2000)
  expect(signIn.error.value).toBe('')
  expect(signInProblem(signIn.flow.value)).toBe('Access denied')

  vi.mocked(api).mockResolvedValueOnce({ cancelled: true }).mockRejectedValueOnce(new ApiError('This plugin has no device sign-in.', 400))
  await signIn.start()
  expect(signIn.flow.value).toBeNull()
  expect(signIn.error.value).toBe('This plugin has no device sign-in.')
})
