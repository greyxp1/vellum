{self}: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.vellum;
  tomlFormat = pkgs.formats.toml {};
in {
  options.services.vellum = {
    enable = lib.mkEnableOption "vellum, a live screen annotation overlay for Wayland";
    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      description = "Vellum package to use.";
    };
    settings = lib.mkOption {
      type = tomlFormat.type;
      default = {};
      example = {
        default_tool = "arrow";
        remember_last_tool = false;
        default_fill_shapes = true;
        feedback_duration_ms = 250;
        tools.pen.opacity = 0.75;
        palette = [
          "#FF6B6B"
          "#FFD93D"
          "#6BCB77"
          "#4D96FF"
          "#845EC2"
        ];
      };
      description = ''
        Configuration options for vellum.
        See available options at <https://github.com/greyxp1/vellum#configuration>.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [cfg.package];
    xdg.configFile."vellum/config.toml" = lib.mkIf (cfg.settings != {}) {
      source = tomlFormat.generate "vellum-config.toml" cfg.settings;
    };
    systemd.user.services.vellum = {
      Unit = {
        Description = "Vellum screen annotation overlay";
        After = ["graphical-session.target"];
        PartOf = ["graphical-session.target"];
      };
      Service = {
        ExecStart = "${cfg.package}/bin/vellum";
        Restart = "on-failure";
      };
      Install.WantedBy = ["graphical-session.target"];
    };
  };
}
