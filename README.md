# Vellum

`vellum` is a small native Wayland overlay for drawing directly over the live desktop,
tested on niri.

https://github.com/user-attachments/assets/f8171063-16a8-497f-ba20-9e11bc50727e

Vellum began as a fork of [Chameleos](https://github.com/Treeniks/chameleos) by Thomas Lindae.

## Usage

Start the overlay once, then toggle drawing from a compositor shortcut:

```sh
vellum &
vellum toggle
```

For example, in niri:

```kdl
Mod+A { spawn "vellum" "toggle"; }
```

## Controls

### Mouse

| Input | Action |
| --- | --- |
| Left drag | Draw or manipulate the selection |
| Right-click | Open the radial picker |
| Hold right click, move, then release | Choose a tool or color |
| Middle-click | Toggle eraser mode |
| Middle drag | Erase annotations |
| Mouse back button | Undo |
| Mouse forward button | Redo |
| Mouse wheel | Change stroke width or text size |

### Trackpad

| Input | Action |
| --- | --- |
| One-finger drag | Draw or manipulate the selection |
| Right click (two-finger click) | Open the radial picker |
| Three-finger tap | Toggle eraser mode |
| Two-finger scroll | Change stroke width or text size |

### Pen

| Input | Action |
| --- | --- |
| Drag with the pen tip | Draw or manipulate the selection |
| Briefly press the barrel button while hovering | Open the radial picker |
| Hold the barrel button while hovering, move, then release | Choose a tool or color |
| Hold the barrel button while drawing | Temporarily erase until released |
| Drag with the eraser tip | Erase annotations |

### Shortcuts and modifiers

| Input | Action |
| --- | --- |
| `Ctrl+Z` | Undo |
| `Ctrl+Shift+Z` or `Ctrl+Y` | Redo |
| `Ctrl+A` | Select all annotations |
| `Backspace` or `Delete` | Delete selected annotations |
| `Escape` | Cancel, clear the selection, or leave drawing mode |
| `Ctrl` + click in selection mode | Add or remove an annotation from the selection |
| Double-click selected text | Edit it |
| `Shift` while drawing | Constrain the shape |
| `Alt` while drawing | Draw triangles, rectangles, and ellipses from their center |
| `F` | Toggle between outlined and filled shapes |
| `Ctrl` + scroll | Change opacity |
| `Shift` + scroll | Change roundness |

Drag selection handles to reshape supported elements.

Run `vellum --help` or `man vellum` for startup options and commands.

## Configuration

See [Configuration](docs/configuration.md) for the available options, defaults, and file lookup
order.

## Home Manager

```nix
{
  imports = [inputs.vellum.homeModules.default];
  services.vellum.enable = true;
}
```

## Building from source

### Arch

For a manual build:

```sh
sudo pacman -S --needed base-devel git rust wayland libxkbcommon vulkan-icd-loader
```

For a PKGBUILD:

```bash
depends=('wayland' 'libxkbcommon' 'vulkan-icd-loader')
makedepends=('cargo')
```

### Debian 13+

```sh
sudo apt install build-essential git pkg-config rustup libwayland-dev libxkbcommon-dev libvulkan1
rustup default stable
```

### Fedora

```sh
sudo dnf install cargo gcc git pkgconf-pkg-config wayland-devel libxkbcommon-devel vulkan-loader
```

Then build:

```sh
git clone https://github.com/greyxp1/vellum
cd vellum
cargo build --release --locked
```

With Nix, skip the system dependencies and run `nix build`, or use `nix develop` for a
development shell.
