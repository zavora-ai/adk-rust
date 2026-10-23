- Allow Linux OS-sandboxed drivers built by Nix to start with their dynamic loader and
  shared libraries. Bubblewrap exposes the existing `/nix/store` read-only alongside its
  system runtime directories; driver write permissions remain limited to their scratch
  home and declared `allow_write` paths.
