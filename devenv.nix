{ pkgs, lib, ... }:

{
  packages = with pkgs; [
    pkg-config
    openssl
    openssl.dev
  ];

  languages.rust = {
    enable = true;
    channel = "stable";
    components = [ "rustc" "cargo" "clippy" "rustfmt" "rust-analyzer" ];
  };

  env.PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig";
  env.OPENSSL_DIR = "${pkgs.openssl.dev}";
  env.OPENSSL_LIB_DIR = "${pkgs.openssl.out}/lib";
  env.OPENSSL_INCLUDE_DIR = "${pkgs.openssl.dev}/include";

  env.LD_LIBRARY_PATH = lib.optionalString pkgs.stdenv.isLinux (
    lib.makeLibraryPath [
      pkgs.stdenv.cc.cc.lib
      pkgs.openssl.out
      pkgs.zlib
    ]
  );

  enterShell = ''
    echo "polyfill-rs dev environment"
    echo "  rust:    $(rustc --version 2>/dev/null)"
    echo "  cargo:   $(cargo --version 2>/dev/null)"
    echo "  openssl: $(pkg-config --modversion openssl 2>/dev/null)"
    echo ""
    echo "Run tests:"
    echo "  cargo test --all-features"
    echo "  cargo test --all-features --test integration_v2 -- --ignored"
  '';

  enterTest = ''
    cargo test --all-features --lib
  '';
}
