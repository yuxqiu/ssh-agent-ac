{
  description = "ssh-agent-ac Rust project as a multi-system flake with dev shell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        ssh-agent-ac = pkgs.rustPlatform.buildRustPackage {
          pname = "ssh-agent-ac";
          version = "0.1.0";
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
        };
      in
      {
        # Buildable Rust package
        packages = {
          ssh-agent-ac = ssh-agent-ac;
          default = ssh-agent-ac;
        };

        # Make it runnable with `nix run`
        apps.ssh-agent-ac = {
          type = "app";
          program = "${ssh-agent-ac}/bin/ssh-agent-ac";
        };
        apps.default = self.apps.${system}.ssh-agent-ac;

        # Development shell
        devShells.default = pkgs.mkShell {
          buildInputs = [
            pkgs.rustc
            pkgs.cargo
            self.packages.${system}.ssh-agent-ac
          ];
        };
      }
    );
}
