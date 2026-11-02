export function validationScope(files) {
  const shared = files.some(file => /^(?:crates\/contracts\/|packages\/contracts\/|scripts\/contracts-kotlin\/|scripts\/generate-contracts\.mjs$|tests\/fixtures\/|apps\/android\/app\/src\/main\/java\/build\/cairn\/(?:contracts|boundary)\/)/.test(file))
  const tooling = files.some(file => /^(?:\.github\/|package\.json$|pnpm-lock\.yaml$|scripts\/(?:validate-pr|pr-validation-scope|pr-validation-required)\.mjs$)/.test(file))
  const browser = tooling
    || files.some(file => file.startsWith('apps/web/')
      || file.startsWith('crates/')
      || file.startsWith('packages/contracts/')
      || file.startsWith('tests/e2e/')
      || file.startsWith('tests/fixtures/')
      || ['Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'scripts/generate-contracts.mjs', 'scripts/qualify-beacon-sql-fixture.ts', 'scripts/test-environment.mjs', 'scripts/tests/validate-pr.test.mjs'].includes(file))
  return {
    android: shared || tooling || files.some(file => file.startsWith('apps/android/')),
    contracts: shared || tooling || files.some(file => file.startsWith('crates/') || ['Cargo.lock', 'Cargo.toml', 'rust-toolchain.toml'].includes(file)),
    browser,
  }
}
