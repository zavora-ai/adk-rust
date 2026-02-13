{ pkgs, ... }:

{
  packages = [
    pkgs.git
    pkgs.glib.dev
    pkgs.pkg-config
    pkgs.openssl
    pkgs.libva
    pkgs.sccache
    pkgs.wild
  ];

  env = {
    PKG_CONFIG_PATH = "${pkgs.glib.dev}/lib/pkgconfig";
    RUSTC_WRAPPER = "sccache";
    # Use wild linker for faster builds
    RUSTFLAGS = "-C link-arg=-fuse-ld=wild";
  };

  languages.rust.enable = true;
  languages.rust.channel = "stable";

  # Optional: Enable pre-commit hooks
  pre-commit.hooks.rustfmt.enable = true;
  pre-commit.hooks.clippy.enable = true;
}
