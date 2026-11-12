# Vendored nsh skills

Copied unmodified from <https://github.com/longbridge/nsh/tree/v0.5.0/skills> at the
`v0.5.0` tag (`f4d68f7f9f31eb7b3ecc8a4cbc31096d5c6238a1`, 2026-11-11), the release the
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
