{ bun2nix, pkgs, ... }:
let
  bunDeps = bun2nix.fetchBunDeps {
    bunNix = ./bun.nix;
  };
  buildExecutable = pname: module: bun2nix.mkDerivation {
    inherit pname module bunDeps;
    version = "0.1.0";
    src = ./.;
  };
in
pkgs.symlinkJoin {
  name = "personal-intake-0.1.0";
  paths = [
    (buildExecutable "intake" "src/cli.ts")
    (buildExecutable "intake-fastmail-source" "src/sources/fastmail.ts")
    (buildExecutable "intake-github-source" "src/sources/github.ts")
  ];
}
