# NixOS module for Spectral
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
