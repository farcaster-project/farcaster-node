let
  pkgs = import
    (builtins.fetchGit {
      name = "nixos-release-22.05";
      url = "https://github.com/nixos/nixpkgs/";
      ref = "refs/heads/release-22.05";
      rev = "6ddd2d34e3701339eb01bbf1259b258adcc6156c";
    })
    {
      overlays = [
        (import (builtins.fetchTarball
          "https://github.com/oxalica/rust-overlay/archive/master.tar.gz"))
      ];
    };
in
pkgs.mkShell {
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
