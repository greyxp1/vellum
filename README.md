# Vellum

Vellum is a small native Wayland overlay for drawing directly over the live desktop.

https://github.com/user-attachments/assets/f8171063-16a8-497f-ba20-9e11bc50727e

Vellum began as a fork of [Chameleos](https://github.com/Treeniks/chameleos) by Thomas Lindae.

## Compatibility

Vellum requires a Wayland compositor that implements [`wlr-layer-shell`](https://wayland.app/protocols/wlr-layer-shell-unstable-v1). This includes niri, Sway, Hyprland, river, Wayfire, labwc, KDE Plasma, and COSMIC, though niri is the only compositor currently tested.

## Installation

### NixOS

For released versions, use the [NixOS module from nixpkgs](https://search.nixos.org/options?channel=unstable&query=vellum&type=options).

For Vellum main, use the flake module:

```nix
{
  imports = [inputs.vellum.nixosModules.default];
  services.vellum = {
    enable = true;
    # Reduce driver memory use on NVIDIA. Disables software fallback.
    # graphicsDriver = "nvidia";
  };
}
```

### From source

Install the dependencies for your distribution:

<details>
<summary>Arch dependencies</summary>

```sh
sudo pacman -S --needed base-devel git rust wayland libxkbcommon fontconfig mesa vulkan-icd-loader
```

</details>

<details>
<summary>Debian 13+ dependencies</summary>

```sh
sudo apt install build-essential git pkg-config rustup libwayland-dev libxkbcommon-dev libfontconfig-dev libgbm-dev libvulkan1
rustup default stable
```

</details>

<details>
<summary>Fedora dependencies</summary>

```sh
sudo dnf install cargo gcc git pkgconf-pkg-config wayland-devel libxkbcommon-devel fontconfig-devel mesa-libgbm-devel vulkan-loader
```

</details>

Build and install to `~/.local`:

```sh
git clone https://github.com/greyxp1/vellum
cd vellum
cargo xtask install
```

## Usage

Run `vellum &` and bind `vellum toggle` to a compositor shortcut. For example, in niri:

```kdl
Mod+A { spawn "vellum" "toggle"; }
```

<details>
<summary>Running as a systemd user service</summary>

Save this as `~/.config/systemd/user/vellum.service`:

```ini
[Unit]
Description=Vellum screen annotation overlay
After=graphical-session.target
PartOf=graphical-session.target

[Service]
Type=exec
ExecStart=%h/.local/bin/vellum
Restart=on-failure

[Install]
WantedBy=graphical-session.target
```

Enable the service:

```sh
systemctl --user enable --now vellum.service
```

</details>
