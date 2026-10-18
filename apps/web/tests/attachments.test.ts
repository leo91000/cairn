import { describe, expect, it } from 'vitest'
import {
  arrange,
  badgeColor,
  badgeLabel,
  detail,
  documentPreview,
  extension,
  family,
  familyColors,
  familyExtensions,
  mosaic,
  needsSummary,
  size,
  splitName,
  subtitle,
  validateSelection,
} from '../src/attachments'

const MB = 1024 * 1024
const file = (name: string, mediaType = 'application/octet-stream', kind: 'image' | 'file' = 'file') => ({ name, mediaType, kind })

describe('chat attachment families', () => {
  // The exact table shared with Android.
  it('mirrors the shared extension table', () => {
    expect(familyExtensions).toEqual({
      image: 'png jpg jpeg gif webp svg heic heif avif bmp'.split(' '),
      pdf: ['pdf'],
      sheet: 'csv tsv xls xlsx ods numbers'.split(' '),
      doc: 'doc docx odt rtf pages'.split(' '),
      slides: 'ppt pptx odp key'.split(' '),
      code: 'js mjs cjs ts tsx jsx py rs kt kts java go rb php c h cc cpp hpp cs swift sh bash zsh fish sql html htm css scss sass less vue svelte json jsonl yaml yml toml xml ini gradle lua dart scala r ps1 dockerfile'.split(' '),
      text: 'txt log md markdown rst'.split(' '),
      archive: 'zip tar gz tgz bz2 xz 7z rar zst'.split(' '),
      audio: 'mp3 wav m4a ogg oga flac aac opus'.split(' '),
      video: 'mp4 mov webm mkv avi m4v'.split(' '),
    })
    expect(familyColors).toEqual({
      image: null,
      pdf: '#D2452F',
      sheet: '#1F7A4D',
      doc: '#2D62B3',
      slides: '#C4581F',
      code: '#3B3A46',
      text: '#8986A4',
      archive: '#B9822A',
      audio: '#7A4FB0',
      video: '#B23A6E',
      other: '#6D6A78',
    })
  })

  it('maps every listed extension, case-insensitively', () => {
    for (const [name, extensions] of Object.entries(familyExtensions)) {
      for (const ext of extensions) {
        if (ext.length <= 8)
          expect(family(file(`report.${ext.toUpperCase()}`)), ext).toBe(name)
      }
    }
  })

  it('reads the extension only after a non-empty base and when it is 1–8 alphanumerics', () => {
    expect(extension('Brief.v3.PDF')).toBe('pdf')
    expect(extension('.gitignore')).toBe('')
    expect(extension('Makefile')).toBe('')
    expect(extension('archive.')).toBe('')
    expect(extension('notes.backup-1')).toBe('')
    expect(extension('data.verylongext')).toBe('')
    expect(extension('a.12345678')).toBe('12345678')
    expect(extension('a.123456789')).toBe('')
  })

  it('trusts the image kind, then the extension, then the media type', () => {
    expect(family(file('scan.bin', 'image/png', 'image'))).toBe('image')
    expect(family(file('photo.pdf', 'image/jpeg', 'image'))).toBe('image')
    // The server reports every non-image upload as octet-stream: the extension decides.
    expect(family(file('Brief.pdf'))).toBe('pdf')
    expect(family(file('export.csv'))).toBe('sheet')
    expect(family(file('main.rs'))).toBe('code')
    expect(family(file('README'))).toBe('other')
    expect(family(file('README', 'text/plain'))).toBe('text')
    expect(family(file('scan', 'application/pdf'))).toBe('pdf')
    expect(family(file('voice.memo', 'audio/mpeg'))).toBe('audio')
    expect(family(file('clip.raw', 'video/mp4'))).toBe('video')
    expect(family(file('pasted', 'image/png'))).toBe('image')
    expect(family(file('blob.bin', 'application/zip'))).toBe('other')
    // A known extension wins over the media type.
    expect(family(file('notes.md', 'application/pdf'))).toBe('text')
  })

  it('labels badges with at most four uppercase characters, or nothing', () => {
    expect(badgeLabel('Brief.pdf')).toBe('PDF')
    expect(badgeLabel('notes.markdown')).toBe('MARK')
    expect(badgeLabel('data.jsonl')).toBe('JSON')
    expect(badgeLabel('Dockerfile')).toBe('')
    expect(badgeColor(file('Brief.pdf'))).toBe('#D2452F')
    expect(badgeColor(file('photo.png'))).toBe('#6D6A78')
    expect(badgeColor(file('unknown.qqq'))).toBe('#6D6A78')
  })
})

describe('attachment names and subtitles', () => {
  it('keeps the extension apart for a middle ellipsis', () => {
    expect(splitName('Brief_refonte_page_tarifs_v3_FINAL.pdf')).toEqual({ base: 'Brief_refonte_page_tarifs_v3_FINAL', ext: '.pdf' })
    expect(splitName('archive.tar.gz')).toEqual({ base: 'archive.tar', ext: '.gz' })
    expect(splitName('.env')).toEqual({ base: '.env', ext: '' })
    expect(splitName('LICENSE')).toEqual({ base: 'LICENSE', ext: '' })
  })

  it('formats sizes like the rest of the web app', () => {
    expect(size(812)).toBe('812 B')
    expect(size(248 * 1024 - 10)).toBe('248 KB')
    expect(size(1.8 * MB)).toBe('1.8 MB')
  })

  it('adds pages and lines when the summary knows them', () => {
    const pdf = { ...file('Brief.pdf'), size: 812 * 1024 }
    const csv = { ...file('export.csv'), size: 248 * 1024 }
    expect(subtitle(pdf, { pages: 12 })).toBe('PDF · 12 pages · 812 KB')
    expect(subtitle(pdf, { pages: 1 })).toBe('PDF · 1 page · 812 KB')
    expect(subtitle(csv, { lines: 1204, excerpt: 'id,name' })).toBe('CSV · 1,204 lines · 248 KB')
    expect(subtitle({ ...file('main.rs'), size: 3000 }, { lines: 1 })).toBe('RS · 1 line · 3 KB')
    expect(subtitle(csv)).toBe('CSV · 248 KB')
    expect(subtitle(csv, null)).toBe('CSV · 248 KB')
    expect(subtitle(csv, { lines: 3 }, { details: false })).toBe('CSV · 248 KB')
    expect(subtitle({ ...file('LICENSE'), size: 10 })).toBe('File · 10 B')
    expect(subtitle({ ...file('LICENSE'), size: 10 }, null, { label: false })).toBe('10 B')
    // Pages only describe PDFs; lines only sheets, text and code.
    expect(detail(file('notes.md'), { pages: 3 })).toBe('')
    expect(detail(file('Brief.pdf'), { lines: 3 })).toBe('')
    expect(detail(file('archive.zip'), { lines: 3 })).toBe('')
  })

  it('previews PDFs as pages and csv/tsv, code and text as excerpts', () => {
    expect(documentPreview(file('Brief.pdf'))).toBe('page')
    expect(documentPreview(file('export.csv'))).toBe('excerpt')
    expect(documentPreview(file('export.tsv'))).toBe('excerpt')
    expect(documentPreview(file('main.rs'))).toBe('excerpt')
    expect(documentPreview(file('notes.txt'))).toBe('excerpt')
    expect(documentPreview(file('budget.xlsx'))).toBeNull()
    expect(documentPreview(file('deck.pptx'))).toBeNull()
    expect(documentPreview(file('photo.png'))).toBeNull()
    expect(needsSummary(file('budget.xlsx'))).toBe(false)
    expect(needsSummary(file('Brief.pdf'))).toBe(true)
  })

  it('splits a message into a mosaic, document cards and compact cards', () => {
    const items = [file('a.png'), file('Brief.pdf'), file('b.jpg'), file('deck.key'), file('main.rs'), file('song.mp3')]
    const groups = arrange(items)
    expect(groups.images.map(item => item.name)).toEqual(['a.png', 'b.jpg'])
    expect(groups.documents.map(item => item.name)).toEqual(['Brief.pdf', 'main.rs'])
    expect(groups.files.map(item => item.name)).toEqual(['deck.key', 'song.mp3'])
  })
})

describe('mosaic layout', () => {
  it('chooses the layout and the hidden count', () => {
    expect(mosaic(0)).toEqual({ layout: 'single', shown: 0, hidden: 0 })
    expect(mosaic(1)).toEqual({ layout: 'single', shown: 1, hidden: 0 })
    expect(mosaic(2)).toEqual({ layout: 'pair', shown: 2, hidden: 0 })
    expect(mosaic(3)).toEqual({ layout: 'triple', shown: 3, hidden: 0 })
    expect(mosaic(4)).toEqual({ layout: 'grid', shown: 4, hidden: 0 })
    expect(mosaic(7)).toEqual({ layout: 'grid', shown: 4, hidden: 3 })
  })
})

describe('attachment selection', () => {
  it('keeps over 8 files a global error', () => {
    const selection = validateSelection(Array.from({ length: 6 }, () => ({ size: 1 })), [{ size: 1 }, { size: 1 }, { size: 1 }])
    expect(selection.error).toBe('Attach up to 8 files per message.')
  })

  it('rejects files over 10 MB with their size', () => {
    expect(validateSelection([], [{ size: 14 * MB }, { size: 10 * MB }])).toEqual({ rejected: ['Too large (14.0 MB > 10 MB)', null] })
  })

  it('rejects the files that would take the message past 40 MB, ignoring rejected ones', () => {
    const draft = [{ size: 9 * MB }, { size: 9 * MB }, { size: 9 * MB }, { size: 50 * MB, rejected: true }]
    expect(validateSelection(draft, [{ size: 9 * MB }, { size: 5 * MB }, { size: 4 * MB }])).toEqual({
      rejected: [null, 'Message would exceed 40 MB', null],
    })
    expect(validateSelection([{ size: 30 * MB }], [{ size: 10 * MB }])).toEqual({ rejected: [null] })
  })
})
