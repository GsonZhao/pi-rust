# rpi 0.1.12

This release aligns provider/model configuration and package updates more
closely with native Pi while keeping update operations fail-closed.

## Highlights

- Read `models.json` as an ordered provider/model catalog and apply native Pi's
  authenticated default-selection rules without leaking first-party API keys
  into custom compatible providers.
- Use Pi-compatible managed npm and Git package stores, structured
  `npmCommand` argv, npm aliases, trusted project settings, bounded subprocesses,
  and conservative package provenance checks.
- Check rpi and package updates independently at startup, so a slow package
  manager cannot delay the rpi update notice. `PI_OFFLINE=1`, `true`, or `yes`
  disables network work across startup and manual update commands.
- Stage Git package updates before atomically replacing the installed checkout;
  build or activation failures preserve the previous package.
- Stage and validate self-updates before replacing the running executable on
  Unix and Windows. Windows background replacement failures are reported on the
  next startup instead of being silently lost.
- Install, update, and uninstall Rust-native extension artifacts under one
  mutation lock with rollback for ordinary filesystem and registry failures.
- Add a resumable, clean-tree release workflow for the nine published crates.

## Compatibility

- Minimum supported Rust remains 1.78.
- npm support remains optional at runtime. Invalid `npmCommand` configuration
  disables npm work without suppressing Git, Rust-native, or rpi update checks.
- Existing legacy package locations remain discoverable; local package paths
  continue to be enabled in place rather than copied or deleted.

## Known Limitation

Rust-native multi-file artifact transactions recover from ordinary reported
errors, but do not yet keep a crash journal. A process or system termination in
the narrow interval between filesystem renames can require manual repair of the
extension registry or artifact directory.
