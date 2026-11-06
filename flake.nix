{
  description = "catmail - a cross-platform email client";

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
          # What catmail-gui links against: nsh's GTK 4 backend and the web views of bodies.
          guiLibs = with pkgs; [
            gtk4
            webkitgtk_6_0
          ];
          # Link with mold: GNU ld turns a one-line change to catmail-gui into a slow
          # rebuild, mostly relinking debug info; mold is several times faster. Only in the
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
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath guiLibs;
            env = linkWithMold;
            # Unset means the default directories, which must stay. GTK's GSettings
            # schemas (catmail-gui's file dialogs abort without them) are where
            # wrapGAppsHook would point a packaged app.
            shellHook = ''
              export XDG_DATA_DIRS=${pkgs.gtk4}/share/gsettings-schemas/${pkgs.gtk4.name}:${pkgs.gsettings-desktop-schemas}/share/gsettings-schemas/${pkgs.gsettings-desktop-schemas.name}:''${XDG_DATA_DIRS:-/usr/local/share:/usr/share}
            '';
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
