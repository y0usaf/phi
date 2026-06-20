{
  description = "Extension-first Rust coding harness scaffold";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
    }:
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

      mkCrane = system:
        let
          pkgs = mkPkgs system;
          craneLib = crane.mkLib pkgs;

          # crane's default filter strips non-Rust files; we need .py and .json
          # for include_str!() macros in phi-repl, phi-ai, and phi-builtins
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter =
              path: type:
              (craneLib.filterCargoSources path type)
              || (lib.hasSuffix ".py" path)
              || (lib.hasSuffix ".json" path);
          };
        in
        craneLib.buildPackage {
          inherit src;
          pname = "phi-app";
          version = "0.1.0";
          cargoArtifacts = craneLib.buildDepsOnly {
            inherit src;
            pname = "phi-deps";
          };
          cargoExtraArgs = "-p phi-app";
          meta.mainProgram = "phi";
        };
    in
    {
      packages = forAllSystems (
        system:
        let
          phi-app = mkCrane system;
        in
        {
          inherit phi-app;
          default = phi-app;
        }
      );

      apps = forAllSystems (
        system:
        {
          default = {
            type = "app";
            program = "${self.packages.${system}.phi-app}/bin/phi";
          };
        }
      );

      nixosConfigurations.phi = lib.nixosSystem {
        system = "x86_64-linux";
        modules = [
          (
            { pkgs, ... }:
            {
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
              ]
              ++ [ self.packages.x86_64-linux.phi-app ];
              system.stateVersion = "24.11";
            }
          )
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

      formatter = forAllSystems (
        system:
        let
          pkgs = mkPkgs system;
        in
        pkgs.nixfmt
      );
    };
}
