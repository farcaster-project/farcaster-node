let
  pkgs = import (builtins.fetchGit {
    # Descriptive name to make the store path easier to identify
    name = "nixos-release-22.05";
    url = "https://github.com/nixos/nixpkgs/";
    # Commit hash for nixos-unstable as of 2018-09-12
    # `git ls-remote https://github.com/nixos/nixpkgs nixos-unstable`
    ref = "refs/heads/release-22.05";
    rev = "cbaacfb8dfa2ddadfb152fa8ef163b40db9041af";
  })
  # pkgs = import (<nixos>) # or comment pkgs above and uncomment this
    {
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
