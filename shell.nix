let
  pkgs = import (builtins.fetchGit {
    name = "nixos-release-24.05";
    url = "https://github.com/nixos/nixpkgs/";
    ref = "refs/heads/release-24.05";
    rev = "9957cd48326fe8dbd52fdc50dd2502307f188b0d";
  }) {
    overlays = [
      (import (builtins.fetchTarball
        "https://github.com/oxalica/rust-overlay/archive/master.tar.gz"))
    ];
  };
in pkgs.mkShell {
  buildInputs = with pkgs; [
    cargo
    cargo-watch
    rust-analyzer
    rustc
    rustfmt
    openssl
    clang
    cmake
    binutils
    protobuf
    docker
    docker-compose
  ];
  nativeBuildInputs = with pkgs; [ pkg-config ];
}
