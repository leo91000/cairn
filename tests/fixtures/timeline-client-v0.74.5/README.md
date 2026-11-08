# Archived transport clients for update qualification

These three unmodified files come from `apps/web/src` at
`7bbf2390aaa949864cab65aff236a85407ea3bdb` (`v0.74.5`).
`provenance.json` binds their bytes. They are test data, never imported by a shipped app.
The qualification transpiles them in a closed VM with synthetic network/peer adapters,
so it can observe the old client's actual handling of current refusal primitives.
No timeline renderer/reducer or legacy interpretation is copied here.

The historical source and branches remain preserved. These tests cannot retrofit UI
features missing from those binaries; HTTP-only limitations must remain explicit.
