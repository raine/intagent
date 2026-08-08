{
  description = "Local personal intake monitor";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    systems.url = "github:nix-systems/default";
    bun2nix.url = "github:nix-community/bun2nix?ref=2.1.2";
    bun2nix.inputs.nixpkgs.follows = "nixpkgs";
    bun2nix.inputs.systems.follows = "systems";
  };

  nixConfig = {
    extra-substituters = [ "https://nix-community.cachix.org" ];
    extra-trusted-public-keys = [
      "nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs="
    ];
  };

  outputs = inputs:
    let
      eachSystem = inputs.nixpkgs.lib.genAttrs (import inputs.systems);
      pkgsFor = eachSystem (system: import inputs.nixpkgs {
        inherit system;
        overlays = [ inputs.bun2nix.overlays.default ];
      });
      packageSetFor = eachSystem (system:
        pkgsFor.${system}.callPackage ./default.nix { }
      );
    in
    {
      packages = eachSystem (system: {
        default = packageSetFor.${system}.bun;
        bun = packageSetFor.${system}.bun;
        browser = packageSetFor.${system}.browser;
        rust = packageSetFor.${system}.rust;
        rust-compat = pkgsFor.${system}.callPackage ./rust-compat.nix { };
      });

      apps = eachSystem (system: {
        default = {
          type = "app";
          program = "${inputs.self.packages.${system}.default}/bin/intake";
        };
        rust = {
          type = "app";
          program = "${inputs.self.packages.${system}.rust}/bin/intake";
        };
      });

      checks = eachSystem (system: {
        bun-package = inputs.self.packages.${system}.bun;
        browser-package = inputs.self.packages.${system}.browser;
        rust-package = inputs.self.packages.${system}.rust;
        rust-compat = inputs.self.packages.${system}.rust-compat;
      });

      devShells = eachSystem (system: {
        default = pkgsFor.${system}.mkShell {
          packages = with pkgsFor.${system}; [
            bun
            bun2nix
            cargo
            clippy
            just
            nodejs
            rust-analyzer
            rustc
            rustfmt
            shellcheck
          ];
          RUST_SRC_PATH = "${pkgsFor.${system}.rustPlatform.rustLibSrc}";
        };
      });
    };
}
