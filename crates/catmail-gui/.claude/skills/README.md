# Vendored nsh skills

Copied unmodified from <https://github.com/longbridge/nsh/tree/v0.6.0/skills> at the
`v0.6.0` tag (`554163758e30d2f62f814096a6327937dcb4fe49`, 2026-11-18), the release the
workspace `Cargo.toml` uses:

- `nsh-design-guides`: the Design Guides, for any change to a screen, layout, spacing,
  color, component, interaction state or interface copy.
- `nsh`: the Coding Guides, Rust components, reactivity and testing.

catmail-gui is a Rust app that uses nsh as a library and brings its own shell
(`src/shell/`): it has no `nsh.json` and isn't built with the `nsh` CLI, so the parts of
`nsh` about TSX, `nsh init`, packaging and updates don't apply.

Copyright 2026 Longbridge, licensed under the Apache License 2.0
([LICENSE-APACHE](LICENSE-APACHE)).

To update, replace both directories with the upstream versions at the release the
workspace uses, and record its tag and commit here.
