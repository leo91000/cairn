import { Buffer } from 'node:buffer'
import filesystem from 'node:fs/promises'
import { join, resolve } from 'node:path'

const ioCodes = new Set(['EACCES', 'EDQUOT', 'EEXIST', 'EIO', 'EISDIR', 'EMFILE', 'ENFILE', 'ENOENT', 'ENOSPC', 'ENOTDIR', 'EPERM'])

function record(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    && [Object.prototype, null].includes(Object.getPrototypeOf(value))
}

function pathString(value) {
  return typeof value === 'string' && value.length > 0 && !value.includes('\0')
}

function blocked(reason, detail = {}) {
  return {
    incidentPath: null,
    pressure: true,
    reason,
    ...detail,
  }
}

function protectionPaths(coordinator, pendingNotification, relatedNotifications) {
  const paths = []
  const addPaths = (value, field) => {
    if (value === undefined)
      return
    if (!Array.isArray(value))
      return field
    for (const path of value) {
      if (!pathString(path))
        return field
      paths.push(path)
    }
  }

  const addNotification = (value, field) => {
    if (!record(value))
      return field
    if (value.incidentPath === undefined || value.incidentPath === null)
      return
    if (!pathString(value.incidentPath))
      return `${field}.incidentPath`
    paths.push(value.incidentPath)
  }

  let invalid = addPaths(coordinator.retainedAlerts, 'retainedAlerts')
  if (invalid)
    return { invalid }
  if (coordinator.incidents !== undefined && !Array.isArray(coordinator.incidents))
    return { invalid: 'incidents' }
  for (const incident of coordinator.incidents ?? []) {
    if (!record(incident))
      return { invalid: 'incidents[]' }
    invalid = addPaths(incident.evidencePaths, 'incidents[].evidencePaths')
    if (invalid)
      return { invalid }
  }

  if (pendingNotification !== undefined && pendingNotification !== null) {
    invalid = addNotification(pendingNotification, 'pendingNotification')
    if (invalid)
      return { invalid }
  }

  if (!Array.isArray(relatedNotifications))
    return { invalid: 'relatedNotifications' }
  for (const notification of relatedNotifications) {
    invalid = addNotification(notification, 'relatedNotifications[]')
    if (invalid)
      return { invalid }
  }

  return { paths }
}

// The archive is optional: callers must persist their full outbox before calling
// this helper and keep that evidence when incidentPath is null. One writer owns
// the directory. This function never edits or acknowledges the caller's state.
export async function retainAlert(dir, eventId, value, coordinator = {}, pendingNotification, relatedNotifications = []) {
  if (!pathString(dir) || typeof eventId !== 'string' || !/^[\dTZ-]{1,80}$/.test(eventId))
    return blocked('invalid_archive')
  if (!record(coordinator))
    return blocked('invalid_protection', { field: 'coordinator' })
  if (coordinator.monitor !== undefined && !record(coordinator.monitor))
    return blocked('invalid_budget', { field: 'monitor' })
  const maxFiles = coordinator.monitor?.maxAlertFiles === undefined ? 32 : coordinator.monitor.maxAlertFiles
  const maxBytes = coordinator.monitor?.maxAlertBytes === undefined ? 8 * 1024 * 1024 : coordinator.monitor.maxAlertBytes
  if (!Number.isSafeInteger(maxFiles) || maxFiles < 1 || !Number.isSafeInteger(maxBytes) || maxBytes < 1)
    return blocked('invalid_budget')

  // Validate every source before flattening it. Unknown protection means no
  // pruning, even when some of the remaining paths happen to be valid.
  const protection = protectionPaths(coordinator, pendingNotification, relatedNotifications)
  if (protection.invalid)
    return blocked('invalid_protection', { field: protection.invalid })
  const directory = resolve(dir)
  const protectedPaths = new Set(protection.paths.map(path => resolve(directory, path)))
  let content
  try {
    const serialized = JSON.stringify(value, null, 2)
    if (serialized === undefined)
      return blocked('archive_serialization')
    content = `${serialized}\n`
  }
  catch {
    return blocked('archive_serialization')
  }

  const incomingBytes = Buffer.byteLength(content)
  if (incomingBytes > maxBytes)
    return blocked('archive_budget')

  let stage = 'list'
  let temporaryPath
  let temporaryHandle
  let ownsTemporary = false
  let result
  try {
    const files = []
    for (const entry of await filesystem.readdir(directory, { withFileTypes: true })) {
      if (!entry.isFile() || !/^alert-[-\dTZ]+\.json$/.test(entry.name))
        continue
      const path = join(directory, entry.name)
      stage = 'stat'
      const info = await filesystem.stat(path)
      files.push({ path, bytes: info.size, mtime: info.mtimeMs })
    }

    files.sort((a, b) => a.mtime - b.mtime || a.path.localeCompare(b.path))
    const incidentPath = join(directory, `alert-${eventId}.json`)
    stage = 'destination'
    try {
      await filesystem.lstat(incidentPath)
      return blocked('archive_conflict')
    }
    catch (error) {
      if (error.code !== 'ENOENT')
        throw error
    }

    let bytes = files.reduce((sum, file) => sum + file.bytes, 0)
    let count = files.length
    const removals = []
    for (const file of files) {
      if (count + 1 <= maxFiles && bytes + incomingBytes <= maxBytes)
        break
      if (protectedPaths.has(file.path))
        continue
      removals.push(file)
      count--
      bytes -= file.bytes
    }

    if (count + 1 > maxFiles || bytes + incomingBytes > maxBytes)
      return blocked('archive_budget', { retainedFiles: files.length, retainedBytes: files.reduce((sum, file) => sum + file.bytes, 0) })

    // Stage the incoming evidence before deleting anything. A failed write must
    // not discard old archives for an event that could never be published.
    stage = 'write'
    temporaryPath = `${incidentPath}.tmp`
    temporaryHandle = await filesystem.open(temporaryPath, 'wx', 0o600)
    ownsTemporary = true
    await filesystem.writeFile(temporaryHandle, content)
    await temporaryHandle.close()
    temporaryHandle = undefined
    stage = 'prune'
    for (const file of removals)
      await filesystem.unlink(file.path)
    stage = 'rename'
    await filesystem.rename(temporaryPath, incidentPath)
    temporaryPath = undefined
    result = {
      incidentPath,
      pressure: false,
      retainedFiles: count + 1,
      retainedBytes: bytes + incomingBytes,
    }
  }
  catch (error) {
    result = blocked('archive_io', { stage, code: ioCodes.has(error.code) ? error.code : 'UNKNOWN' })
  }
  finally {
    if (temporaryHandle) {
      try {
        await temporaryHandle.close()
      }
      catch (error) {
        if (result)
          result.cleanupCode = ioCodes.has(error.code) ? error.code : 'UNKNOWN'
      }
    }

    if (ownsTemporary && temporaryPath) {
      try {
        await filesystem.unlink(temporaryPath)
      }
      catch (error) {
        if (error.code !== 'ENOENT' && result)
          result.cleanupCode = ioCodes.has(error.code) ? error.code : 'UNKNOWN'
      }
    }
  }

  return result
}
