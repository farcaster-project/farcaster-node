let
  pkgs = import <nixpkgs> {};
in
  pkgs.mkShell {
    buildInputs = with pkgs; [
        openssl
        cargo
        cargo-watch
        rustup
        rust-analyzer
        clang
        binutils
        cmake
        gcc
        monero
    ];
    nativeBuildInputs = with pkgs; [
        pkg-config
    ];
  }
