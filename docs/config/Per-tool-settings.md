### Built-in tool defaults

| Tool | Size | Roundness | System cursor | Scrolling defaults |
| --- | --- | --- | --- | --- |
| `pen` | `stroke_size` | `1` | Off | |
| `line` | `stroke_size` | `0.5` | On | |
| `arrow` | `stroke_size` | `0.25` | Off | |
| `triangle` | `stroke_size` | `0` | On | |
| `rectangle` | `stroke_size` | `0.1` | On | |
| `ellipse` | `stroke_size` |  | On | |
| `text` | `16` | `0.1` | On | `size_range = { min = 8, max = 500 }`<br>`steps.size = 0.5` |
| `eraser` | `10` |  | Off | |
| `select` |  |  | On | |

### `[tools.<name>]`

| Property | Built-in default | Supported primitives | Supported values |
| --- | --- | --- | --- |
| `primitive` | Required | All | `pen`, `line`, `arrow`, `triangle`, `rectangle`, `ellipse`, `text`, `eraser`, `select` |
| `size` | Global `stroke_size` or built-in default above | All except `select` | Logical pixels within `size_range`. |
| `roundness` | See table above | `pen`, `line`, `arrow`, `triangle`, `rectangle`, `text` | 0 (sharp) to 1 (round). |
| `opacity` | Alpha of `default_color` | All except `eraser` and `select` | 0.05–1. |
| `filled` | `default_fill_shapes` | `triangle`, `rectangle`, `ellipse` | Boolean. |
| `background` | `false` | `text` | Boolean. Adds an automatic black or white background. |
| `cursor.visible` | Global setting or preset above | All | Boolean. |
| `icon` | Matching icon | All | A primitive name, `"marker"`, or a table with an `svg` path relative to the config. |
| `font.family` | `"sans-serif"` | `text` | Ordered [CSS font-family list](https://developer.mozilla.org/en-US/docs/Web/CSS/Reference/Properties/font-family#values) |
| `font.weight` | `400` | `text` | [Weight](https://developer.mozilla.org/en-US/docs/Web/CSS/Reference/Properties/font-weight#common_weight_name_mapping) from 1 to 1000. |
| `font.style` | `"normal"` | `text` | `"normal"`, `"italic"`, or `"oblique"`. |
| `font.features` | `{}` | `text` | [OpenType feature tags](https://sparanoid.com/lab/opentype-features/) (`0` disables, `1` enables). For example, `zero = 1`, `tnum = 1`, `liga = 0` |
