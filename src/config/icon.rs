use std::path::{Path, PathBuf};

use kurbo::{Affine, BezPath};
use peniko::Fill;
use tiny_skia_path::{Path as SvgPath, PathSegment};

use crate::tool::Icon;

#[derive(Debug, serde::Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub(super) enum IconConfig {
    Named(Icon),
    File { svg: PathBuf },
}

pub(crate) enum ToolIcon {
    Named(Icon),
    Svg(SvgIcon),
}

pub(crate) struct SvgIcon {
    pub size: [f64; 2],
    pub commands: Vec<SvgCommand>,
}

pub(crate) enum SvgCommand {
    Fill {
        path: BezPath,
        rule: Fill,
        opacity: f32,
    },
    PushClip {
        path: BezPath,
        rule: Fill,
    },
    PopClip,
}

impl IconConfig {
    pub(super) fn resolve(self, directory: &Path) -> Result<ToolIcon, String> {
        match self {
            Self::Named(icon) => Ok(ToolIcon::Named(icon)),
            Self::File { svg } => {
                let path = directory.join(svg);
                read_svg(&path).map_err(|error| format!("{}: {error}", path.display()))
            }
        }
    }
}

fn read_svg(path: &Path) -> Result<ToolIcon, String> {
    let source = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let options = usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(&source, &options).map_err(|error| error.to_string())?;
    let width = f64::from(tree.size().width());
    let height = f64::from(tree.size().height());
    let scale = 19.68 / width.max(height);
    let normalize = Affine::scale(scale) * Affine::translate((-width * 0.5, -height * 0.5));
    let mut commands = Vec::new();
    append_group(tree.root(), normalize, 1.0, &mut commands)?;
    if !commands
        .iter()
        .any(|command| matches!(command, SvgCommand::Fill { .. }))
    {
        return Err("SVG has no visible vector shapes".into());
    }
    Ok(ToolIcon::Svg(SvgIcon {
        size: [width * scale, height * scale],
        commands,
    }))
}

fn append_group(
    group: &usvg::Group,
    normalize: Affine,
    opacity: f32,
    commands: &mut Vec<SvgCommand>,
) -> Result<(), String> {
    let opacity = opacity * group.opacity().get();
    if opacity == 0.0 {
        return Ok(());
    }
    if group.mask().is_some() || !group.filters().is_empty() {
        return Err("flatten SVG masks and filters before using this icon".into());
    }
    let clips = group
        .clip_path()
        .map(|clip| push_clips(clip, normalize * affine(group.abs_transform()), commands))
        .transpose()?
        .unwrap_or(0);
    for node in group.children() {
        match node {
            usvg::Node::Group(group) => append_group(group, normalize, opacity, commands)?,
            usvg::Node::Path(path) if path.is_visible() => {
                let transform = normalize * affine(path.abs_transform());
                if let Some(fill) = path.fill().filter(|fill| fill.opacity().get() > 0.0) {
                    commands.push(SvgCommand::Fill {
                        path: bezier(path.data(), transform),
                        rule: fill_rule(fill),
                        opacity: opacity * fill.opacity().get(),
                    });
                }
                if let Some(stroke) = path.stroke().filter(|stroke| stroke.opacity().get() > 0.0) {
                    let stroke_style = stroke.to_tiny_skia();
                    let dashed = stroke_style
                        .dash
                        .as_ref()
                        .and_then(|dash| path.data().dash(dash, 1.0));
                    if let Some(outline) = dashed
                        .as_ref()
                        .unwrap_or(path.data())
                        .stroke(&stroke_style, 1.0)
                    {
                        commands.push(SvgCommand::Fill {
                            path: bezier(&outline, transform),
                            rule: Fill::NonZero,
                            opacity: opacity * stroke.opacity().get(),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    commands.extend((0..clips).map(|_| SvgCommand::PopClip));
    Ok(())
}

fn push_clips(
    mut clip: &usvg::ClipPath,
    space: Affine,
    commands: &mut Vec<SvgCommand>,
) -> Result<usize, String> {
    let mut count = 0;
    loop {
        let mut children = clip.root().children();
        while let [usvg::Node::Group(group)] = children {
            if group.clip_path().is_some() {
                break;
            }
            children = group.children();
        }
        let [usvg::Node::Path(path)] = children else {
            return Err(
                "flatten SVG clip paths with multiple or nested shapes before using this icon"
                    .into(),
            );
        };
        let transform = space * affine(clip.transform()) * affine(path.abs_transform());
        commands.push(SvgCommand::PushClip {
            path: if path.is_visible() {
                bezier(path.data(), transform)
            } else {
                BezPath::new()
            },
            rule: path.fill().map_or(Fill::NonZero, fill_rule),
        });
        count += 1;
        let Some(next) = clip.clip_path() else {
            return Ok(count);
        };
        clip = next;
    }
}

fn fill_rule(fill: &usvg::Fill) -> Fill {
    match fill.rule() {
        usvg::FillRule::NonZero => Fill::NonZero,
        usvg::FillRule::EvenOdd => Fill::EvenOdd,
    }
}

fn affine(transform: tiny_skia_path::Transform) -> Affine {
    Affine::new(
        [
            transform.sx,
            transform.ky,
            transform.kx,
            transform.sy,
            transform.tx,
            transform.ty,
        ]
        .map(f64::from),
    )
}

fn bezier(path: &SvgPath, transform: Affine) -> BezPath {
    let point = |point: tiny_skia_path::Point| {
        transform * kurbo::Point::new(f64::from(point.x), f64::from(point.y))
    };
    let mut result = BezPath::new();
    for segment in path.segments() {
        match segment {
            PathSegment::MoveTo(end) => result.move_to(point(end)),
            PathSegment::LineTo(end) => result.line_to(point(end)),
            PathSegment::QuadTo(control, end) => result.quad_to(point(control), point(end)),
            PathSegment::CubicTo(first, second, end) => {
                result.curve_to(point(first), point(second), point(end));
            }
            PathSegment::Close => result.close_path(),
        }
    }
    result
}
