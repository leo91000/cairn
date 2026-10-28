export function validationScope(files) {
  const shared = files.some(file => /^(?:crates\/contracts\/|packages\/contracts\/|scripts\/contracts-kotlin\/|scripts\/generate-contracts\.mjs$|tests\/fixtures\/|apps\/android\/app\/src\/main\/java\/build\/cairn\/(?:contracts|boundary)\/)/.test(file))
  const tooling = files.some(file => /^(?:\.github\/|package\.json$|pnpm-lock\.yaml$|scripts\/(?:validate-pr|pr-validation-scope)\.mjs$)/.test(file))
  return {
    android: shared || tooling || files.some(file => file.startsWith('apps/android/')),
    contracts: shared || tooling || files.some(file => file.startsWith('crates/') || ['Cargo.lock', 'Cargo.toml', 'rust-toolchain.toml', 'scripts/backend-schemas.mjs'].includes(file)),
  }
}
