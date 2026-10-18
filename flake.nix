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
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              clippy
              rustfmt
              rust-analyzer
              pkg-config
              sqlite-interactive
            ];

            RUST_SRC_PATH = pkgs.rustPlatform.rustLibSrc;
          };
        in
        {
          inherit default;

          # The default shell plus Dovecot, for testenv/dovecot/run.sh.
          imap = pkgs.mkShell {
            inputsFrom = [ default ];
            packages = [ pkgs.dovecot ];
            inherit (default) RUST_SRC_PATH;
          };
        }
      );

      formatter = forAllSystems (pkgs: pkgs.nixfmt-tree);
    };
}
