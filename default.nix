{ buildNpmPackage, importNpmLock, lib, rustPlatform, ... }:
let
  cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
  version = cargoToml.package.version;

  browser = buildNpmPackage {
    pname = "intagent-dashboard";
    inherit version;
    src = ./web;
    npmDeps = importNpmLock {
      npmRoot = ./web;
    };
    npmConfigHook = importNpmLock.npmConfigHook;
    npmBuildScript = "build:bundle";
    installPhase = ''
      runHook preInstall
      mkdir -p "$out/share/intagent/dashboard"
      cp generated/app.js generated/app.css "$out/share/intagent/dashboard/"
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
      ];
    };
    cargoLock = {
      lockFile = ./Cargo.lock;
      outputHashes = {
        "rig-agent-0.41.0" = "sha256-DnMWea4uaRZzozIvpbsfJGrlWsAhkqJrsAA+cm4mbsU=";
      };
    };
    INTAGENT_DASHBOARD_DIR = "${browser}/share/intagent/dashboard";
    doCheck = false;
    postInstall = ''
      mkdir -p "$out/share/doc/intagent/examples"
      cp -R ${./examples/skills} "$out/share/doc/intagent/examples/skills"
    '';
    meta = {
      inherit (cargoToml.package) description;
      license = lib.licenses.mit;
      mainProgram = "intagent";
    };
  };
in
{
  inherit application browser;
}
