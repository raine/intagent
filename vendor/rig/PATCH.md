# Rig source patch

This directory contains `rig-agent`, `rig-core`, and `rig-derive` from the
revision recorded in `REVISION`. The root manifest supplies the workspace values
required to compile the vendored crates.

The `rig-core` patch writes the ChatGPT OAuth cache through a private temporary
file, syncs it, atomically renames it, syncs the parent directory, and rejects
symlink or non-file destinations. This keeps renewable credentials in a
mode-0600 regular file and makes interrupted writes leave the previous cache
intact.
