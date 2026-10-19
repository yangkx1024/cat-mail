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

          default = pkgs.mkShell {
            packages =
              with pkgs;
              [
                cargo
                rustc
                clippy
                rustfmt
                rust-analyzer
                pkg-config
                sqlite-interactive
              ]
              ++ guiLibs;

            RUST_SRC_PATH = pkgs.rustPlatform.rustLibSrc;
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (guiLibs ++ guiRuntimeLibs);
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
          };
        }
      );

      formatter = forAllSystems (pkgs: pkgs.nixfmt-tree);
    };
}
