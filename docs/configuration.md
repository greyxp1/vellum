# Configuration

Vellum reads `~/.config/vellum/config.toml` by default. It respects `$XDG_CONFIG_HOME` and,
when no user config exists, checks `$XDG_CONFIG_DIRS` (defaulting to
`/etc/xdg/vellum/config.toml`). Use `--config PATH` to select another file or `--no-config`
to load no file.

## Options

| Option | Type | Default | Description |
| --- | --- | --- | --- |
| `default_tool` | string | `"pen"` | Tool selected on startup: `pen`, `line`, `arrow`, `triangle`, `rectangle`, `ellipse`, `text`, `eraser`, or `select` |
| `remember_last_tool` | boolean | `true` | Keep the selected tool when drawing mode is reopened |
| `stroke_width` | number | `5.0` | Initial size for pen and shape tools; each keeps its own adjusted size during the session |
| `default_color` | string | First palette color | Initial `#RRGGBB` color; must be present in `palette` |
| `palette` | array of strings | Eight standard colors | Between 2 and 12 `#RRGGBB` colors |
| `feedback_duration_ms` | integer | `500` | How long property feedback remains visible, from `0` to `60000` milliseconds |
| `clear_on_escape` | boolean | `false` | Clear annotations when Escape deactivates drawing mode |
| `default_fill_shapes` | boolean | `false` | Initially fill triangles, rectangles, and ellipses |
| `tools.<tool>.size` | number | Tool default | Initial size for that tool |
| `tools.<tool>.opacity` | number | `1.0` | Initial opacity from `0.05` to `1.0` |
| `tools.<tool>.roundness` | number | Tool default | Initial roundness from `0.0` to `1.0` |
| `tools.<tool>.filled` | boolean | `false` | Whether a fillable shape starts filled |

Per-tool values override `stroke_width` and `default_fill_shapes`. Supported properties are:

| Tool | Properties |
| --- | --- |
| `pen` | `size`, `opacity`, `roundness` |
| `line`, `arrow` | `size`, `opacity`, `roundness` |
| `triangle`, `rectangle` | `size`, `opacity`, `roundness`, `filled` |
| `ellipse` | `size`, `opacity`, `filled` |
| `text` | `size`, `opacity` |
| `eraser` | `size` |
| `select` | None |

## Defaults

```toml
default_tool = "pen"
remember_last_tool = true
stroke_width = 5.0
default_color = "#E84046"
feedback_duration_ms = 500
clear_on_escape = false
default_fill_shapes = false
palette = [
  "#E84046",
  "#EC8948",
  "#EED049",
  "#3ED73C",
  "#0283FC",
  "#7C57EB",
  "#FFFFFF",
  "#000000",
]

[tools.pen]
size = 5.0
opacity = 1.0
roundness = 1.0

[tools.line]
size = 5.0
opacity = 1.0
roundness = 0.5

[tools.arrow]
size = 5.0
opacity = 1.0
roundness = 0.5

[tools.triangle]
size = 5.0
opacity = 1.0
roundness = 0.0
filled = false

[tools.rectangle]
size = 5.0
opacity = 1.0
roundness = 0.05
filled = false

[tools.ellipse]
size = 5.0
opacity = 1.0
filled = false

[tools.text]
size = 20.0
opacity = 1.0

[tools.eraser]
size = 10.0
```
