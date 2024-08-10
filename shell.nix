let
  pkgs = import (builtins.fetchGit {
    name = "nixos-release-24.05";
    url = "https://github.com/nixos/nixpkgs/";
    ref = "refs/heads/release-24.05";
    rev = "5a83f6f984f387d47373f6f0c43b97a64e7755c0";
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
