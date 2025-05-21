{ pkgs ? import (fetchTarball channel:nixos-24.11) {} }:

with pkgs;
stdenv.mkDerivation {
  pname = "perfevent-rs";
  version = "0.0.1";
  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.gitTracked ./.;
  };

  buildInputs = [
    rustc
    rust-analyzer
    cargo
  ];

  shellHook=
    ''
      export LOCALE_ARCHIVE="${pkgs.glibcLocales}/lib/locale/locale-archive";
    '';
}
