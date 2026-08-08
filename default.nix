{ bun2nix, buildNpmPackage, importNpmLock, lib, pkgs, rustPlatform, rustc, ... }:
assert lib.assertMsg (rustc.version == "1.94.0")
  "personal-intake requires Rust 1.94.0, but nixpkgs provides ${rustc.version}";
let
  cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
  version = cargoToml.package.version;

  bunDeps = bun2nix.fetchBunDeps {
    bunNix = ./bun.nix;
  };
  buildExecutable = pname: module: bun2nix.mkDerivation {
    inherit pname module bunDeps version;
    src = ./.;
  };
  bun = pkgs.symlinkJoin {
    name = "personal-intake-bun-${version}";
    paths = [
      (buildExecutable "intake" "src/cli.ts")
      (buildExecutable "intake-fastmail-source" "src/sources/fastmail.ts")
      (buildExecutable "intake-github-source" "src/sources/github.ts")
    ];
  };

  browser = buildNpmPackage {
    pname = "personal-intake-dashboard";
    inherit version;
    src = ./web;
    npmDeps = importNpmLock {
      npmRoot = ./web;
    };
    npmConfigHook = importNpmLock.npmConfigHook;
    npmBuildScript = "build";
    installPhase = ''
      runHook preInstall
      mkdir -p "$out/share/intake/dashboard"
      cp generated/app.js generated/app.css "$out/share/intake/dashboard/"
      runHook postInstall
    '';
  };

  rust = rustPlatform.buildRustPackage {
    pname = cargoToml.package.name;
    inherit version;
    src = lib.fileset.toSource {
      root = ./.;
      fileset = lib.fileset.unions [
        ./Cargo.toml
        ./Cargo.lock
        ./build.rs
        ./src
        ./vendor
      ];
    };
    cargoLock = {
      lockFile = ./Cargo.lock;
      # Rig packages resolve through the reviewed source patch under vendor/rig.
      outputHashes = { };
    };
    INTAKE_DASHBOARD_DIR = "${browser}/share/intake/dashboard";
    doCheck = false;
    postInstall = ''
      mkdir -p "$out/share/intake"
      cp -R ${./skills} "$out/share/intake/skills"
    '';
    meta = {
      inherit (cargoToml.package) description;
      license = lib.licenses.mit;
      mainProgram = "intake";
    };
  };
in
{
  inherit browser bun rust;
}
