# Vendored nsh skills

Copied unmodified from <https://github.com/longbridge/nsh/tree/main/skills> at commit
`f8cfd1a650640bd4b2668201d364e08e3b0ab570` (2026-11-06), the revision the workspace
`Cargo.toml` pins:

- `nsh-design-guides`: the Design Guides, for any change to a screen, layout, spacing,
  color, component, interaction state or interface copy.
- `nsh`: the Coding Guides, Rust components, reactivity and testing.

catmail-gui is a Rust app that uses nsh as a library and brings its own shell
(`src/shell/`): it has no `nsh.json` and isn't built with the `nsh` CLI, so the parts of
`nsh` about TSX, `nsh init`, packaging and updates don't apply.

Copyright 2026 Longbridge, licensed under the Apache License 2.0
([LICENSE-APACHE](LICENSE-APACHE)).

To update, replace both directories with the upstream versions at the revision the
workspace pins, and record that commit here.
