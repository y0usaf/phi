{
  description = "Extension-first Rust coding harness scaffold";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system);
      lib = nixpkgs.lib;
      mkPkgs = system: import nixpkgs { inherit system; };
    in
    {
      nixosConfigurations.phi = lib.nixosSystem {
        system = "x86_64-linux";
        modules = [
          ({ pkgs, ... }: {
            networking.hostName = "phi";
            environment.systemPackages = with pkgs; [
              cargo
              clippy
              nixfmt
              python3
              rust-analyzer
              rustc
              rustfmt
              stdenv.cc
            ];
            system.stateVersion = "24.11";
          })
        ];
      };

      devShells = forAllSystems (
        system:
        let
          pkgs = mkPkgs system;
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              clippy
              pkg-config
              nixfmt
              python3
              rust-analyzer
              rustc
              rustfmt
              stdenv.cc
            ];
          };
        }
      );

      apps = forAllSystems (
        system:
        let
          pkgs = mkPkgs system;
          runner = pkgs.writeShellApplication {
            name = "phi-run";
            runtimeInputs = with pkgs; [
              cargo
              rustc
              stdenv.cc
            ];
            text = ''
              exec cargo run --manifest-path "$PWD/Cargo.toml" -p phi-app -- "$@"
            '';
          };
        in
        {
          default = {
            type = "app";
            program = "${runner}/bin/phi-run";
          };
        }
      );

      formatter = forAllSystems (
        system:
        let
          pkgs = mkPkgs system;
        in
        pkgs.nixfmt
      );
    };
}
