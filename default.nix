{ buildNpmPackage, importNpmLock, lib, rustPlatform, rustc, ... }:
assert lib.assertMsg (rustc.version == "1.94.0")
  "personal-intake requires Rust 1.94.0, but nixpkgs provides ${rustc.version}";
let
  cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
  version = cargoToml.package.version;

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

  application = rustPlatform.buildRustPackage {
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
  inherit application browser;
}
