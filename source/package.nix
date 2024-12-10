{ pkgs }: pkgs.callPackage
  ({ lib
   , rustPlatform
   , pkg-config
   , rustc
   , cargo
   , makeWrapper
   }:
  rustPlatform.buildRustPackage rec {
    pname = "puterium";
    version = "0.0.0";
    cargoLock = {
      lockFile = ./puterium/Cargo.lock;
    };
    src = ../source;
    sourceRoot = "source/puterium";
    preConfigure = ''
      cd ../../
      mv source ro
      cp -r ro rw
      chmod -R u+w rw
      cd rw/puterium
    '';
    nativeBuildInputs = [
      pkg-config
      cargo
      rustc
      rustPlatform.bindgenHook
    ];
  })
{ }
