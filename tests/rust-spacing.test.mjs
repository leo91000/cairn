import { execFileSync, spawnSync } from 'node:child_process'
import {
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { spaceRustDeclarations } from '../scripts/rust-spacing.mjs'

describe('rust declaration spacing', () => {
  it('separates declarations while keeping attributes and documentation attached', () => {
    const source = 'use std::fmt;\n/// A value.\n#[derive(Clone)]\nstruct A;\nimpl A {\n    fn first() {}\n    /// Another method.\n    #[inline]\n    fn second() {}\n}\n'
    const expected = 'use std::fmt;\n\n/// A value.\n#[derive(Clone)]\nstruct A;\n\nimpl A {\n    fn first() {}\n\n    /// Another method.\n    #[inline]\n    fn second() {}\n}\n'

    expect(spaceRustDeclarations(source)).toBe(expected)
    expect(spaceRustDeclarations(expected)).toBe(expected)
  })

  it('keeps import, module and short constant groups compact', () => {
    const source = 'use a::A;\nuse b::B;\n\nmod a;\nmod b;\n\nconst A: u8 = 1;\nstatic B: u8 = 2;\n'

    expect(spaceRustDeclarations(source)).toBe(source)
  })

  it('leaves statements, macro bodies and multiline literals byte-for-byte intact', () => {
    const body = 'fn first() {\n    let source = r#"fn example() {}\nstruct Example;\n"#;\n    let other = 2;\n    json!({"source": source,"other":other});\n    tokio::select! { _=ready()=>{}, else=>{} }\n}\n'

    expect(spaceRustDeclarations(body)).toBe(body)
    expect(spaceRustDeclarations(`${body}fn second() {}\n`)).toBe(`${body}\nfn second() {}\n`)
  })

  it('preserves trailing comments and Unicode offsets', () => {
    const source = '// Français 🦀\nstruct A; // trailing\n/// B\nstruct B;\n'
    const expected = '// Français 🦀\nstruct A; // trailing\n\n/// B\nstruct B;\n'

    expect(spaceRustDeclarations(source)).toBe(expected)
    expect(spaceRustDeclarations(expected)).toBe(expected)
  })

  it('handles nested modules, traits and declarations on the same line', () => {
    expect(spaceRustDeclarations('struct A;struct B;')).toBe('struct A;\n\nstruct B;')
    const source = 'mod nested {\n    trait A {\n        fn first();\n        fn second();\n    }\n    struct B;\n}\n'
    const expected = 'mod nested {\n    trait A {\n        fn first();\n\n        fn second();\n    }\n\n    struct B;\n}\n'

    expect(spaceRustDeclarations(source)).toBe(expected)
  })

  it('keeps an existing separator before a leading comment', () => {
    const source = 'struct A;\n\n// Explains B.\nstruct B;\n'

    expect(spaceRustDeclarations(source)).toBe(source)
  })

  it('checks without writing, fixes tracked and new files, and leaves the index untouched', () => {
    const directory = mkdtempSync(path.join(tmpdir(), 'rust-spacing-'))
    const script = fileURLToPath(new URL('../scripts/rust-spacing.mjs', import.meta.url))
    const source = 'struct A;\nstruct B;\n'
    const tracked = path.join(directory, 'tracked.rs')
    const untracked = path.join(directory, 'new.rs')
    const run = args => spawnSync(process.execPath, [script, ...args], { cwd: directory, encoding: 'utf8' })

    try {
      execFileSync('git', ['init', '--quiet', directory])
      writeFileSync(tracked, source)
      writeFileSync(untracked, source)
      execFileSync('git', ['add', 'tracked.rs'], { cwd: directory })

      expect(run(['--check']).status).toBe(1)
      expect(readFileSync(tracked, 'utf8')).toBe(source)
      expect(readFileSync(untracked, 'utf8')).toBe(source)
      expect(run([]).status).toBe(0)
      expect(readFileSync(tracked, 'utf8')).toBe('struct A;\n\nstruct B;\n')
      expect(readFileSync(untracked, 'utf8')).toBe('struct A;\n\nstruct B;\n')
      expect(run(['--check']).status).toBe(0)
      expect(execFileSync('git', ['show', ':tracked.rs'], { cwd: directory, encoding: 'utf8' })).toBe(source)
    }
    finally {
      rmSync(directory, { recursive: true, force: true })
    }
  })
})
