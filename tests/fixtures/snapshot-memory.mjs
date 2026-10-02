import fs from 'node:fs'
import process from 'node:process'

const group = '/run/leo-cgroup/leo-shared'
const vmms = fs.readdirSync('/proc').filter(pid => /^\d+$/.test(pid)).flatMap((pid) => {
  try {
    if (fs.readFileSync(`/proc/${pid}/comm`, 'utf8').trim() !== 'firecracker')
      return []
    const raw = fs.readFileSync(`/proc/${pid}/smaps_rollup`, 'utf8')
    const memory = Object.fromEntries(raw.split('\n').flatMap((line) => {
      const field = line.match(/^(Rss|Pss|Pss_Anon|Pss_File|Private_Dirty|Shared_Dirty|Shared_Clean|Swap):\s+(\d+) kB/)
      return field ? [[field[1], Number(field[2])]] : []
    }))
    const mappings = fs.readFileSync(`/proc/${pid}/maps`, 'utf8').split('\n').filter(line => line.includes('snapshot.mem'))
    return [{ pid, memory, mappings }]
  }
  catch { return [] }
})
process.stdout.write(`${JSON.stringify({
  at: Date.now(),
  currentBytes: Number(fs.readFileSync(`${group}/memory.current`, 'utf8')),
  stat: fs.readFileSync(`${group}/memory.stat`, 'utf8'),
  events: fs.readFileSync(`${group}/memory.events`, 'utf8'),
  vmms,
})}\n`)
