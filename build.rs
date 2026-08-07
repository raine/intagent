use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=INTAKE_DASHBOARD_DIR");

    let source_directory = env::var_os("INTAKE_DASHBOARD_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("web/generated")
        });
    let output_directory = PathBuf::from(env::var_os("OUT_DIR").unwrap());

    for asset in ["app.js", "app.css"] {
        let source = source_directory.join(asset);
        println!("cargo:rerun-if-changed={}", source.display());
        copy_asset(&source, &output_directory.join(asset));
    }
}

fn copy_asset(source: &Path, destination: &Path) {
    if !source.is_file() {
        panic!(
            "required dashboard asset {} is missing; run `npm ci --prefix web && npm run build --prefix web` or set INTAKE_DASHBOARD_DIR to a directory containing app.js and app.css",
            source.display()
        );
    }
    fs::copy(source, destination).unwrap_or_else(|error| {
        panic!(
            "failed to copy dashboard asset {} to {}: {error}",
            source.display(),
            destination.display()
        )
    });
}
