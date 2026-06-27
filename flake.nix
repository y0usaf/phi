{
  description = "Extension-first Rust coding harness scaffold";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane = {
      url = "github:ipetkov/crane";
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

      # Shared crane setup for a given system: source filter + dependency
      # artifacts.  Both the package output and the test check derive from
      # the same `cargoArtifacts` so that Nix caches the dependency build
      # across `nix build`, `nix run`, and `nix flake check`.
      mkCraneLib = system:
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

          # Clear rustup env vars so nix's rustc-wrapper doesn't pick up
          # the user's rustup-managed toolchain, which causes "cannot find ld"
          # in the sandbox.
          commonEnv = {
            RUSTUP_HOME = "";
            RUSTUP_TOOLCHAIN = "";
            # Provide lld (the linker rustc uses with -fuse-ld=lld) in the build env
            nativeBuildInputs = [ pkgs.llvmPackages.bintools ];
          };

          cargoArtifacts = craneLib.buildDepsOnly {
            inherit src;
            pname = "phi-deps";
            version = "0.1.0";
            inherit (commonEnv) RUSTUP_HOME RUSTUP_TOOLCHAIN nativeBuildInputs;
          };
        in
        {
          inherit pkgs craneLib src cargoArtifacts commonEnv;
        };

      # The publishable binary: builds only phi-app.
      mkApp = system:
        let
          c = mkCraneLib system;
        in
        c.craneLib.buildPackage {
          inherit (c) src cargoArtifacts;
          pname = "phi-app";
          version = "0.1.0";
          inherit (c.commonEnv) RUSTUP_HOME RUSTUP_TOOLCHAIN nativeBuildInputs;
          cargoExtraArgs = "-p phi-app";
          meta.mainProgram = "phi";
        };

      # A check that compiles and runs `cargo test` for the *entire* workspace
      # (not just phi-app).  Reuses the same dependency artifacts as mkApp so
      # nothing is compiled twice across `nix build` and `nix flake check`.
      mkTest = system:
        let
          c = mkCraneLib system;
        in
        c.craneLib.cargoTest {
          inherit (c) src cargoArtifacts;
          pname = "phi-test";
          version = "0.1.0";
          inherit (c.commonEnv) RUSTUP_HOME RUSTUP_TOOLCHAIN;
          # phi-builtins tests spawn a Python REPL (python3) and run bash()
          # via subprocess(shell=True → /bin/sh); both are available through
          # the sandbox's stdenv, but python3 must be added explicitly.
          nativeBuildInputs = c.commonEnv.nativeBuildInputs ++ [ c.pkgs.python3 ];
          # Test every workspace member, not just phi-app.
          cargoExtraArgs = "--workspace";
        };
    in
    {
      packages = forAllSystems (
        system:
        let
          phi-app = mkApp system;
        in
        {
          inherit phi-app;
          default = phi-app;
        }
      );

      checks = forAllSystems (
        system: {
          workspace-test = mkTest system;
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
