# NixOS module for Spectral
{ overlay }:
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
      # Built here rather than read from `pkgs.spectral`: without this flake's
      # overlay, that attribute is nixpkgs' unrelated Spectral typeface.
      default = (overlay pkgs pkgs).spectral;
      defaultText = lib.literalMD "this flake's Spectral, built against the system's `pkgs`";
      description = "The Spectral package to use.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    services.pipewire.enable = lib.mkDefault true;
  };
}
