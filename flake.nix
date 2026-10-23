{
  description = "NomNom development environment";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in {
      devShells = forAllSystems (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              clippy
              espeak-ng
              pkg-config
              pipewire
              rustc
              rustfmt
              (python3.withPackages (python: with python; [
                kokoro
                numpy
                soundfile
                spacy-models.en_core_web_sm
              ]))
            ];
            buildInputs = with pkgs; [ gtk4 ];
          };
        });
    };
}
