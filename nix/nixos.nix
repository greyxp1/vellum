{self}: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.vellum;
  toml = pkgs.formats.toml {};
  configFile = toml.generate "vellum-config.toml" cfg.settings;
  nvidia = config.hardware.nvidia.package;
in {
  disabledModules = ["services/wayland/vellum.nix"];

  options.services.vellum = {
    enable = lib.mkEnableOption "Vellum, a live screen annotation overlay for Wayland";
    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "inputs.vellum.packages.\${pkgs.stdenv.hostPlatform.system}.default";
      description = "Vellum package to use.";
    };
    settings = lib.mkOption {
      type = toml.type;
      default = {};
      example = {
        default_tool = "arrow";
        tools.pen.opacity = 0.75;
      };
      description = ''
        Configuration written to {file}`/etc/xdg/vellum/config.toml`.
        See <https://github.com/greyxp1/vellum/blob/master/default-config.toml>.
      '';
    };
    graphicsDriver = lib.mkOption {
      type = lib.types.enum ["auto" "nvidia"];
      default = "auto";
      description = ''
        Driver discovery for the Vellum service. Selecting NVIDIA avoids
        loading other Vulkan and EGL drivers, reducing memory use. It requires
        the NixOS NVIDIA driver and disables software rendering fallback for Vellum.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [{
      assertion = cfg.graphicsDriver != "nvidia" || lib.elem "nvidia" config.services.xserver.videoDrivers;
      message = "services.vellum.graphicsDriver = \"nvidia\" requires the NixOS NVIDIA driver.";
    }];

    environment.systemPackages = [cfg.package];
    environment.etc."xdg/vellum/config.toml" = lib.mkIf (cfg.settings != {}) {
      source = configFile;
    };
    systemd.user.services.vellum = {
      description = "Vellum screen annotation overlay";
      after = ["graphical-session.target"];
      partOf = ["graphical-session.target"];
      wantedBy = ["graphical-session.target"];
      restartTriggers = lib.optional (cfg.settings != {}) configFile;
      environment = lib.mkIf (cfg.graphicsDriver == "nvidia") {
        VK_DRIVER_FILES = "${nvidia}/share/vulkan/icd.d/nvidia_icd.json";
        __EGL_VENDOR_LIBRARY_FILENAMES = "${nvidia}/share/glvnd/egl_vendor.d/10_nvidia.json";
      };
      serviceConfig = {
        Type = "exec";
        ExecStart = lib.getExe cfg.package;
        Restart = "on-failure";
      };
    };
  };
}
