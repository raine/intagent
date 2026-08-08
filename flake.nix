{
  description = "Local personal intake monitor";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    systems.url = "github:nix-systems/default";
  };

  outputs = inputs:
    let
      eachSystem = inputs.nixpkgs.lib.genAttrs (import inputs.systems);
      pkgsFor = eachSystem (system: import inputs.nixpkgs {
        inherit system;
      });
      packageSetFor = eachSystem (system:
        pkgsFor.${system}.callPackage ./default.nix { }
      );
    in
    {
      packages = eachSystem (system: {
        default = packageSetFor.${system}.application;
        browser = packageSetFor.${system}.browser;
        rust-compat = pkgsFor.${system}.callPackage ./rust-compat.nix { };
      });

      apps = eachSystem (system: {
        default = {
          type = "app";
          program = "${inputs.self.packages.${system}.default}/bin/intake";
        };
      });

      checks = eachSystem (system: {
        application-package = inputs.self.packages.${system}.default;
        browser-package = inputs.self.packages.${system}.browser;
        rust-compat = inputs.self.packages.${system}.rust-compat;
      });

      devShells = eachSystem (system: {
        default = pkgsFor.${system}.mkShell {
          packages = with pkgsFor.${system}; [
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
