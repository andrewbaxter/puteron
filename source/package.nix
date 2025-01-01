{ pkgs }: pkgs.callPackage
  ({ lib
   , rustPlatform
   , pkg-config
   , rustc
   , cargo
   , makeWrapper
   }:
  rustPlatform.buildRustPackage rec {
    pname = "puteron";
    version = "0.0.0";
    cargoLock = {
      lockFile = ./puteron/Cargo.lock;
    };
    src = ../source;
    sourceRoot = "source/puteron";
    preConfigure = ''
      cd ../../
      mv source ro
      cp -r ro rw
      chmod -R u+w rw
      cd rw/puteron
    '';
    nativeBuildInputs = [
      pkg-config
      cargo
      rustc
      rustPlatform.bindgenHook
    ];
  })
{ }
