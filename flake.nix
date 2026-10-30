{
  description = "cat-mail - a cross-platform email client";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      devShells = forAllSystems (
        pkgs:
        let
          # What catmail-gui (GPUI) links against.
          guiLibs = with pkgs; [
            fontconfig
            freetype
            libxkbcommon
            libxcb
          ];
          # What GPUI loads at runtime with dlopen: the Wayland client and the Vulkan
          # loader (with the GL fallback wgpu tries when there is no Vulkan driver).
          guiRuntimeLibs = with pkgs; [
            wayland
            vulkan-loader
            libGL
            libX11
          ];
          # Link with mold: GNU ld turns a one-line change to catmail-gui into a 25 s
          # rebuild, mostly relinking debug info; mold makes it about 3 s. Only in the
          # shells, so CI and builds outside them keep the default linker. Every shell
          # sets it, or switching between them rebuilds everything.
          linkWithMold = {
            "CARGO_TARGET_${pkgs.stdenv.hostPlatform.rust.cargoEnvVarTarget}_RUSTFLAGS" =
              "-C link-arg=-fuse-ld=mold";
          };

          default = pkgs.mkShell {
            packages =
              with pkgs;
              [
                cargo
                rustc
                clippy
                rustfmt
                rust-analyzer
                cargo-deny
                mold
                pkg-config
                sqlite-interactive
              ]
              ++ guiLibs;

            RUST_SRC_PATH = pkgs.rustPlatform.rustLibSrc;
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (guiLibs ++ guiRuntimeLibs);
            env = linkWithMold;
          };
        in
        {
          inherit default;

          # The default shell plus the real servers for testenv/dovecot/run.sh.
          testenv = pkgs.mkShell {
            inputsFrom = [ default ];
            packages = [
              pkgs.dovecot
              pkgs.mailpit
              pkgs.openssl
            ];
            inherit (default) RUST_SRC_PATH LD_LIBRARY_PATH;
            env = linkWithMold;
          };
        }
      );

      formatter = forAllSystems (pkgs: pkgs.nixfmt-tree);
    };
}
