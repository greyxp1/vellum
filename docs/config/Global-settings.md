| Option | Default | What it changes |
| --- | --- | --- |
| `draw_on` | `"all"` | Draw on all monitors or only the `"current"` monitor. |
| `default_tool` | `"pen"` | Tool name at startup. |
| `tool_wheel` | `["pen", "line", "arrow", "triangle", "rectangle", "ellipse", "select", "text"]` | 1–16 unique tool names in wheel order. |
| `stroke_size` | `5`, clamped to `size_range` | Starting size in logical pixels for pen, line, arrow, and shapes. |
| `default_fill_shapes` | `false` | Fill triangles, rectangles, and ellipses. |
| `default_color` | First palette color | Starting [CSS color](https://developer.mozilla.org/en-US/docs/Web/CSS/Reference/Values/color_value#syntax) from `palette`. |
| `palette` | `["#E84046", "#EF8F4F", "#EED14D", "#4DD54F", "#0483FA", "#7C58EA", "#EBEBEB", "#141414"]` | 2–12 [CSS colors](https://developer.mozilla.org/en-US/docs/Web/CSS/Reference/Values/color_value#syntax) with alpha of at least 0.05. |
| `cursor.visible` | `true` | Show the system cursor. |
| `annotation_shadows` | `true` | Draw shadows behind annotations. |
| `annotation_shadow_overlap` | `false` | Allow shadows over other annotations. |
| `whiteboard.color` | `"#EBEBEB"` | Whiteboard background (opaque CSS color). |
| `feedback_duration_ms` | `500` | Adjustment indicator duration (0–60000 ms). Zero hides it. |
| `remember_last_tool` | `true` | Keep the selected tool between drawing sessions. |
| `remember_tool_properties` | `true` | Keep color and tool adjustments between drawing sessions. |
| `clear_on_cancel` | `false` | Clear annotations when Cancel exits drawing mode. |
| `freeze_on_activate` | `false` | Freeze the screen when entering drawing mode. |
| `annotations_visible` | `true` | Show annotations outside drawing mode. |

### Presentation

| Option | Default | Applies when | What it changes |
| --- | --- | --- | --- |
| `presentation.remember` | `true` |  | Keep presentation mode on between drawing sessions. |
| `presentation.hold_ms` | `3000` |  | Wait before removing an annotation (0–60000 ms). |
| `presentation.fade_duration_ms` | `1000` |  | Annotation fade time (1–60000 ms). |
| `presentation.pen_effect` | `"fade"` |  | `"fade"` for pen strokes or `"erase"` along their paths. |
| `presentation.erase_duration_ms` | `"drawn"` | `presentation.pen_effect = "erase"` | Use the drawing time or a fixed duration (1–60000 ms). |
| `presentation.erase_pace` | `"recorded"` | `presentation.pen_effect = "erase"` | Match the drawing pace or use `"constant"`. |

### Scrolling

| Option | Default | Supported values |
| --- | --- | --- |
| `size_range` | `{ min = 1, max = 100 }` | Set `min`, `max`, or both in logical pixels. Values must be positive and finite, with `min` no greater than `max`. |
| `steps.size` | `1` | Positive finite increment. |
| `steps.opacity`<br>`steps.roundness` | `0.01` | Positive finite increments. |
| `stops.size`<br>`stops.opacity`<br>`stops.roundness` | `[]` | Size in logical pixels within `size_range`.<br>Opacity 0.05–1.<br>Roundness 0–1.<br>Scrolling pauses at these stops and the tool’s configured starting value. |
