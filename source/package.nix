{ pkgs, debug ? false }: pkgs.callPackage
  ({ lib
   , rustPlatform
   , pkg-config
   , rustc
   , cargo
   , makeWrapper
   }:
  let
    naersk = pkgs.callPackage (fetchTarball "https://github.com/nix-community/naersk/archive/378614f37a6bee5a3f2ef4f825a73d948d3ae921.zip") { };
    buildRust = { root, extra ? { } }:
      let
        # https://github.com/nix-community/naersk/issues/133
        fixCargoPaths = src:
          let
            cargoToml = lib.importTOML (src + "/Cargo.toml");
            isPathDep = v: (builtins.isAttrs v) && builtins.hasAttr "path" v;
            newCargoToml = cargoToml // {
              dependencies = builtins.mapAttrs
                (n: v:
                  if (isPathDep v) && (lib.hasInfix ".." v.path) then
                    v // { path = fixCargoPaths (src + "/${v.path}"); }
                  else
                    v
                )
                cargoToml.dependencies;
            };
            newCargoTomlFile = (pkgs.formats.toml { }).generate "Cargo.toml" newCargoToml;
            propagatedBuildInputs = lib.mapAttrsToList
              (n: p: p.path)
              (lib.filterAttrs
                (n: v: (isPathDep v) && (! (lib.isString v.path)))
                newCargoToml.dependencies
              );
          in
          pkgs.runCommand "${lib.last (lib.splitString "/" src)}-fixed-paths"
            { propagatedBuildInputs = propagatedBuildInputs; }
            ''
              cp -r ${src} $out
              chmod +w "$out/Cargo.toml"
              cat < ${newCargoTomlFile} > "$out/Cargo.toml"
            '';
        newRoot = fixCargoPaths root;
      in
      naersk.buildPackage (extra // rec {
        src = newRoot;
        propagatedBuildInputs = newRoot.propagatedBuildInputs;
        release = !debug;
      });
  in
  buildRust {
    root = ./puteron-bin;
    extra = {
      nativeBuildInputs = [ pkgs.makeWrapper ];
      postInstall = ''
        wrapProgram $out/bin/puteron --prefix PATH : $out/bin
        rm $out/bin/generate_jsonschema
      '';
    };
  })
{ }
