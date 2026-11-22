# NixOS module for Spectral. Imported by path it reads `pkgs.spectral`, which
# is this app only under the flake's overlay; the flake's nixosModules wrap
# this file and supply the package themselves.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.spectral;
in
{
  options.programs.spectral = {
    enable = lib.mkEnableOption "Spectral audio noise suppression";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.spectral;
      defaultText = lib.literalExpression "pkgs.spectral";
      description = "The Spectral package to use.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    services.pipewire.enable = lib.mkDefault true;
  };
}
