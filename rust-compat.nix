{ lib, rustPlatform }:
let
  cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
in
rustPlatform.buildRustPackage {
  pname = "${cargoToml.package.name}-rig-compat";
  inherit (cargoToml.package) version;

  src = lib.cleanSource ./.;
  cargoLock = {
    lockFile = ./Cargo.lock;
    outputHashes = {
      "rig-agent-0.41.0" = "sha256-5LwLeMHNhNGWnXGISLp1/L9iZoh910ru8zxD9rMEngg=";
    };
  };

  cargoBuildFlags = [ "--lib" "--examples" ];
  cargoTestFlags = [ "--all-targets" ];

  meta = {
    inherit (cargoToml.package) description;
    license = lib.licenses.mit;
  };
}
