import type { SkillArchiveChunk, SkillFile } from '../../../packages/contracts/contracts'
import type { DraftFile } from '../src/skill-bundle'
import { describe, expect, it } from 'vitest'
import {
  base64Bytes,
  base64ToBytes,
  buildSkillTree,
  bytesToBase64,
  countChanges,
  decodeText,
  downloadArchive,
  draftFromBundle,
  draftState,
  formatBytes,
  limitProblems,
  normalizePath,
  shortVersion,
  skillChanges,
  skillFileKind,
  stageChanges,
  stagedWrites,
  syncManifestName,
  validateSkillPath,
  visibleTreeRows,
} from '../src/skill-bundle'

function file(path: string, size = 10, executable = false, text = true): SkillFile {
  return {
    path,
    size,
    executable,
    text,
  }
}

describe('skill bundle files', () => {
  it('formats sizes compactly', () => {
    expect(formatBytes(0)).toBe('0 B')
    expect(formatBytes(812)).toBe('812 B')
    expect(formatBytes(5120)).toBe('5 KB')
    expect(formatBytes(1536)).toBe('1.5 KB')
    expect(formatBytes(16 * 1024 * 1024)).toBe('16 MB')
  })

  it('round-trips base64 and detects text like the backend', () => {
    const bytes = new Uint8Array([0, 1, 2, 250, 255, 128])
    const encoded = bytesToBase64(bytes)
    expect(base64ToBytes(encoded)).toEqual(bytes)
    expect(base64Bytes(encoded)).toBe(bytes.length)
    expect(base64Bytes(bytesToBase64(new Uint8Array(4)))).toBe(4)
    expect(decodeText(new TextEncoder().encode('#!/bin/sh\necho é\n'))).toBe('#!/bin/sh\necho é\n')
    expect(decodeText(new Uint8Array([104, 0, 105]))).toBeNull()
    expect(decodeText(new Uint8Array([0xFF, 0xFE]))).toBeNull()
  })

  it('normalizes and validates paths', () => {
    expect(normalizePath(' ./scripts//check.sh ')).toBe('scripts/check.sh')
    expect(normalizePath('\\assets\\logo.png')).toBe('assets/logo.png')
    expect(validateSkillPath('scripts/check.sh')).toBeNull()
    expect(validateSkillPath('')).toMatch(/Enter/)
    expect(validateSkillPath('../escape.md')).toMatch(/inside/)
    expect(validateSkillPath('scripts/.env')).toMatch(/dot/)
    expect(validateSkillPath('a/b/c/d/e/f/g/h/i.md')).toMatch(/levels/)
    expect(validateSkillPath(`${'x'.repeat(241)}`)).toMatch(/bytes/)
  })

  it('preserves a UTF-8 BOM when decoding an upload', () => {
    const bytes = new Uint8Array([239, 187, 191, 104, 105])
    expect(new TextEncoder().encode(decodeText(bytes)!)).toEqual(bytes)
  })

  it('classifies files for icons and editors', () => {
    expect(skillFileKind(file('SKILL.md'))).toBe('manifest')
    expect(skillFileKind(file('references/notes.md'))).toBe('markdown')
    expect(skillFileKind(file('scripts/check.sh'))).toBe('script')
    expect(skillFileKind(file('bin/run', 10, true))).toBe('script')
    expect(skillFileKind(file('assets/logo.png', 10, false, false))).toBe('image')
    expect(skillFileKind(file('assets/icon.svg'))).toBe('image')
    expect(skillFileKind(file('data/model.bin', 10, false, false))).toBe('binary')
    expect(skillFileKind(file('templates/report.txt'))).toBe('text')
  })
})

describe('skill file tree', () => {
  it('pins SKILL.md, lists folders before files and totals folder sizes', () => {
    const tree = buildSkillTree([
      file('zeta.md', 1),
      file('scripts/b.sh', 2),
      file('SKILL.md', 3),
      file('assets/img/logo.png', 4),
      file('scripts/a.sh', 5),
      file('alpha.md', 6),
    ])
    expect(tree.map(node => node.path)).toEqual(['SKILL.md', 'assets', 'scripts', 'alpha.md', 'zeta.md'])
    const scripts = tree[2]!
    expect(scripts.type === 'folder' && scripts.children.map(node => node.name)).toEqual(['a.sh', 'b.sh'])
    expect(scripts.type === 'folder' && scripts.size).toBe(7)
    const assets = tree[1]!
    expect(assets.type === 'folder' && assets.children[0]!.path).toBe('assets/img')
    expect(visibleTreeRows(tree, new Set(['assets'])).map(node => node.path)).toEqual([
      'SKILL.md',
      'assets',
      'scripts',
      'scripts/a.sh',
      'scripts/b.sh',
      'alpha.md',
      'zeta.md',
    ])
  })
})

describe('skill change sets', () => {
  const original = [file('SKILL.md', 20), file('notes.md'), file('scripts/run.py'), file('old.md'), file('a.md'), file('b.md')]

  it('uses valid temporary paths when swapping maximum-length paths', () => {
    const a = 'a'.repeat(240)
    const b = 'b'.repeat(240)
    const saved = [file('SKILL.md'), file(a), file(b)]
    const draft = draftFromBundle(saved)
    draft[1]!.path = b
    draft[2]!.path = a
    const occupied = new Set(saved.map(file => file.path))
    for (const change of skillChanges(saved, draft)) {
      if (change.op !== 'move')
        continue
      expect(validateSkillPath(change.to)).toBeNull()
      expect(occupied.has(change.to)).toBe(false)
      expect(occupied.delete(change.from)).toBe(true)
      occupied.add(change.to)
    }

    expect(occupied).toEqual(new Set(['SKILL.md', a, b]))
  })

  it('allows unchanged legacy files and totals while refusing new growth', () => {
    const limits = {
      fileBytes: 10,
      bundleBytes: 20,
      files: 2,
      uploadBytes: 100,
    }
    const saved = [file('SKILL.md', 5), file('large.bin', 50), file('old.txt', 4)]
    const draft = draftFromBundle(saved)
    draft[0]!.modified = true
    draft[0]!.size = 6
    draft[2]!.size = 3
    draft[2]!.modified = true
    expect(limitProblems(draft, limits, saved)).toEqual([])
    draft[1]!.modified = true
    expect(limitProblems(draft, limits, saved).some(problem => problem.path === 'large.bin')).toBe(true)
    draft[1]!.modified = false
    draft[0]!.size = 7
    expect(limitProblems(draft, limits, saved).some(problem => !problem.path)).toBe(true)
  })

  it('orders deletions, moves, writes and modes into one batch', () => {
    const draft = draftFromBundle(original).filter(entry => entry.path !== 'old.md')
    const edit = (path: string, change: Partial<DraftFile>) => Object.assign(draft.find(entry => entry.path === path)!, change)
    edit('SKILL.md', { content: '---\nname: review\n---\n', modified: true })
    edit('notes.md', { path: 'references/notes.md' })
    edit('scripts/run.py', { executable: true })
    draft.push({
      path: 'scripts/check.sh',
      origin: null,
      size: 9,
      text: true,
      executable: true,
      originExecutable: false,
      content: '#!/bin/sh',
      modified: true,
    })
    draft.push({
      path: 'assets/logo.png',
      origin: null,
      size: 3,
      text: false,
      executable: false,
      originExecutable: false,
      data: 'AAEC',
      modified: true,
    })

    expect(skillChanges(original, draft)).toEqual([
      { op: 'delete', path: 'old.md' },
      { op: 'move', from: 'notes.md', to: 'references/notes.md' },
      {
        op: 'write',
        path: 'SKILL.md',
        content: '---\nname: review\n---\n',
        executable: false,
      },
      {
        op: 'write',
        path: 'scripts/check.sh',
        content: '#!/bin/sh',
        executable: true,
      },
      {
        op: 'write',
        path: 'assets/logo.png',
        content: 'AAEC',
        encoding: 'base64',
        executable: false,
      },
      { op: 'mode', path: 'scripts/run.py', executable: true },
    ])
    expect(countChanges(original, draft)).toBe(6)
    expect(draft.map(draftState)).toEqual(['modified', 'moved', 'mode', null, null, 'new', 'new'])
  })

  it('swaps two files through a temporary name and lets a new file take a moved path', () => {
    const draft = draftFromBundle(original)
    draft.find(entry => entry.path === 'a.md')!.path = 'b.md'
    draft.find(entry => entry.origin === 'b.md')!.path = 'a.md'
    draft.find(entry => entry.path === 'old.md')!.path = 'archive/old.md'
    draft.push({
      path: 'old.md',
      origin: null,
      size: 1,
      text: true,
      executable: false,
      originExecutable: false,
      content: 'x',
      modified: true,
    })

    const changes = skillChanges(original, draft)
    expect(changes).toEqual([
      { op: 'move', from: 'old.md', to: 'archive/old.md' },
      { op: 'move', from: 'a.md', to: 'cairn-move-1' },
      { op: 'move', from: 'b.md', to: 'a.md' },
      { op: 'move', from: 'cairn-move-1', to: 'b.md' },
      {
        op: 'write',
        path: 'old.md',
        content: 'x',
        executable: false,
      },
    ])
  })

  it('frees a file before a folder takes its name, and a folder before a file takes its name', () => {
    const files = [file('SKILL.md'), file('zeta'), file('alpha/x.md'), file('beta/y.md'), file('notes')]
    const draft = draftFromBundle(files)
    const rename = (from: string, to: string) => draft.find(entry => entry.path === from)!.path = to
    rename('alpha/x.md', 'zeta/x.md')
    rename('zeta', 'zeta.md')
    rename('notes', 'beta')
    rename('beta/y.md', 'archive/y.md')

    expect(skillChanges(files, draft)).toEqual([
      { op: 'move', from: 'zeta', to: 'zeta.md' },
      { op: 'move', from: 'alpha/x.md', to: 'zeta/x.md' },
      { op: 'move', from: 'beta/y.md', to: 'archive/y.md' },
      { op: 'move', from: 'notes', to: 'beta' },
    ])
  })

  it('turns a file into a folder of the same name through a temporary name', () => {
    const files = [file('SKILL.md'), file('guide')]
    const draft = draftFromBundle(files)
    draft.find(entry => entry.path === 'guide')!.path = 'guide/index.md'
    expect(skillChanges(files, draft)).toEqual([
      { op: 'move', from: 'guide', to: 'cairn-move-1' },
      { op: 'move', from: 'cairn-move-1', to: 'guide/index.md' },
    ])
  })

  it('reports nothing for an untouched draft', () => {
    const draft = draftFromBundle(original)
    expect(skillChanges(original, draft)).toEqual([])
    expect(countChanges(original, draft)).toBe(0)
  })

  it('checks limits before saving', () => {
    const limits = {
      fileBytes: 100,
      bundleBytes: 150,
      files: 2,
      uploadBytes: 100,
    }
    const draft = draftFromBundle([file('SKILL.md', 50), file('big.bin', 120, false, false), file('c.md', 5)])
    expect(limitProblems(draft, limits).map(problem => problem.message)).toEqual([
      'big.bin is 120 B; files are limited to 100 B.',
      'This skill would hold 175 B; skills are limited to 150 B.',
      'This skill would hold 3 files; skills are limited to 2.',
    ])
    expect(limitProblems(draftFromBundle([file('notes.md')]), limits)[0]!.message).toMatch(/SKILL.md/)
  })
})

describe('staged uploads', () => {
  it('stages binaries and large text, then the largest writes until the batch fits', () => {
    const changes = [
      { op: 'write' as const, path: 'big.txt', content: 'x'.repeat(2000) },
      { op: 'write' as const, path: 'medium.txt', content: 'y'.repeat(700) },
      { op: 'write' as const, path: 'small.txt', content: 'z'.repeat(100) },
      {
        op: 'write' as const,
        path: 'logo.png',
        content: 'AAEC',
        encoding: 'base64' as const,
      },
      { op: 'delete' as const, path: 'gone.md' },
    ]
    expect([...stagedWrites(changes, { fileBytes: 1500, requestBytes: 1400 })].sort()).toEqual([0, 3])
    expect([...stagedWrites(changes, { fileBytes: 1500, requestBytes: 700 })].sort()).toEqual([0, 1, 3])
    expect([...stagedWrites(changes.slice(1, 3))]).toEqual([])
  })

  it('uploads staged contents as raw bytes and references their ids', async () => {
    const uploaded: Uint8Array[] = []
    const result = await stageChanges([
      {
        op: 'write',
        path: 'large.txt',
        content: 'é'.repeat(10),
        executable: false,
      },
      {
        op: 'write',
        path: 'image.png',
        content: bytesToBase64(new Uint8Array(40).fill(7)),
        encoding: 'base64',
      },
      { op: 'write', path: 'notes.md', content: 'short' },
      { op: 'mode', path: 'run.sh', executable: true },
    ], async (content) => {
      uploaded.push(content)
      return `upload-${uploaded.length}`
    }, { fileBytes: 16 })
    expect(new TextDecoder().decode(uploaded[0])).toBe('é'.repeat(10))
    expect(uploaded[1]).toEqual(new Uint8Array(40).fill(7))
    expect(result).toEqual([
      {
        op: 'write',
        path: 'large.txt',
        upload: 'upload-1',
        executable: false,
      },
      { op: 'write', path: 'image.png', upload: 'upload-2' },
      { op: 'write', path: 'notes.md', content: 'short' },
      { op: 'mode', path: 'run.sh', executable: true },
    ])
  })
})

describe('archive downloads', () => {
  it('follows offsets on the same revision', async () => {
    const archive = new Uint8Array(10).map((_, index) => index)
    const requests: [number, string | undefined][] = []
    const result = await downloadArchive(async (offset, revision): Promise<SkillArchiveChunk> => {
      requests.push([offset, revision])
      return {
        filename: 'review.zip',
        revision: 'r1',
        size: 10,
        offset,
        content: bytesToBase64(archive.slice(offset, offset + 4)),
      }
    })
    expect(requests).toEqual([[0, undefined], [4, 'r1'], [8, 'r1']])
    expect(result.bytes).toEqual(archive)
    expect(result.filename).toBe('review.zip')
  })

  it('stops when the server returns no progress', async () => {
    await expect(downloadArchive(async offset => ({
      filename: 'a.zip',
      revision: 'r',
      size: 10,
      offset,
      content: offset ? '' : bytesToBase64(new Uint8Array(4)),
    }))).rejects.toThrow(/interrupted/)
  })
})

describe('skill sources', () => {
  it('shortens a source version, whatever the plugin calls it', () => {
    expect(shortVersion('0b2ddce3a1b2c3d4e5f60718293a4b5c6d7e8f90')).toBe('0b2ddce')
    expect(shortVersion('v2')).toBe('v2')
  })
})

describe('new skills', () => {
  it('keeps the frontmatter name in step with the skill name', () => {
    const manifest = '---\nname: my-skill\ndescription: Something\n---\n# Body\nname: untouched\n'
    expect(syncManifestName(manifest, 'review')).toBe('---\nname: review\ndescription: Something\n---\n# Body\nname: untouched\n')
    expect(syncManifestName('---\ndescription: x\n---\n', 'review')).toBe('---\nname: review\ndescription: x\n---\n')
    expect(syncManifestName('no frontmatter', 'review')).toBe('no frontmatter')
  })
})
