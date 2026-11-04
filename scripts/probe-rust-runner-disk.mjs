import { execFileSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import process from 'node:process'

const roots = ['/usr/local/lib/android', '/usr/share/dotnet', '/usr/share/swift', '/usr/local/swift', '/opt/ghc', '/opt/hostedtoolcache/CodeQL']
const directories = []
for (const location of roots) {
  try {
    const stat = fs.lstatSync(location)
    directories.push({
      location,
      directory: stat.isDirectory(),
      symlink: stat.isSymbolicLink(),
      uid: stat.uid,
      device: stat.dev,
      realpath: fs.realpathSync(location),
      allocatedBytes: execFileSync('du', ['-x', '--summarize', '-B1', '--', location], { encoding: 'utf8', timeout: 15000 }).trim().split(/\s/)[0],
    })
  }
  catch (error) {
    directories.push({ location, error: error.code ?? error.message })
  }
}

const stat = fs.statfsSync(process.cwd(), { bigint: true })
console.log(JSON.stringify({
  username: os.userInfo().username,
  cpus: os.cpus().length,
  cpu: os.cpus()[0]?.model,
  memoryBytes: os.totalmem(),
  githubActions: process.env.GITHUB_ACTIONS,
  runnerEnvironment: process.env.RUNNER_ENVIRONMENT,
  runnerOS: process.env.RUNNER_OS,
  imageOS: process.env.ImageOS,
  imageVersion: process.env.ImageVersion,
  availableBytes: String(stat.bavail * stat.bsize),
  totalBytes: String(stat.blocks * stat.bsize),
  workspaceDevice: fs.statSync(process.cwd()).dev,
  directories,
}, null, 2))
