{
  description = "sqldiff: semantic diff for T-SQL, PostgreSQL and Db2 for i";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "sqldiff";
          version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter = path: _type: !(pkgs.lib.hasSuffix "/target" path) && baseNameOf path != "result";
          };
          cargoLock.lockFile = ./Cargo.lock;
          # Only the command-line tool; sqldiff-gui and sqldiff-mcp are their
          # own packages in the workspace (release.yml builds all three).
          cargoBuildFlags = [ "-p" "sqldiff-cli" ];
          # Its tests run in checkPhase; the whole workspace is CI's job.
          cargoTestFlags = [ "-p" "sqldiff-cli" ];
          nativeCheckInputs = [ pkgs.git ]; # the changes / drift tests make repositories
          # The --proof tests write a stand-in checker that starts with
          # `#!/usr/bin/env python3`; the build sandbox has no /usr/bin/env.
          postPatch = ''
            substituteInPlace crates/sqldiff-cli/tests/proof.rs \
              --replace-fail '#!/usr/bin/env python3' '#!${pkgs.python3}/bin/python3'
          '';
          meta = {
            description = "Semantic diff for T-SQL, PostgreSQL and Db2 for i";
            mainProgram = "sqldiff";
          };
        };
      });

      apps = forAll (pkgs: {
        default = {
          type = "app";
          program = nixpkgs.lib.getExe self.packages.${pkgs.system}.default;
        };
      });

      checks = forAll (pkgs: {
        default = self.packages.${pkgs.system}.default;
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            rustfmt
            clippy
            rust-analyzer
          ];
        };
      });

      formatter = forAll (pkgs: pkgs.nixfmt);
    };
}
