//! page_render.

use super::{json, ToolResult};
use crate::automation::args::PageRenderArgs;
use crate::automation::{failed, paths, Automation, Content, ToolError};
use crate::commands::render::render_page_rgba;
use crate::state::lock_mutex;
use pdfium_render::prelude::PdfPageIndex;
use serde_json::json;

const DEFAULT_DPI: f64 = 96.0;

/// A render larger than this many pixels is refused (it would be 200 MB of
/// RGBA before encoding). 600 dpi on a large-format page gets there.
const MAX_PIXELS: f64 = 50_000_000.0;

pub(crate) fn page_render(auto: &mut Automation, args: PageRenderArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let page = auto.check_page(args.doc, args.page)?;
    let dpi = args.dpi.unwrap_or(DEFAULT_DPI);

    // The size the page is displayed at (rotation applied), via the page
    // cache, which the render below then reuses.
    let (width_pt, height_pt) = {
        let entry = auto.entry(args.doc)?;
        let mut entry = lock_mutex(&entry)?;
        let pdf_page = entry.page((page - 1) as PdfPageIndex)?;
        (pdf_page.width().value as f64, pdf_page.height().value as f64)
    };
    let scale = dpi / 72.0;
    if (width_pt * scale) * (height_pt * scale) > MAX_PIXELS {
        return Err(ToolError::InvalidArgs(format!(
            "page {page} at {dpi} dpi would be over {} megapixels: use a lower dpi",
            MAX_PIXELS / 1e6
        )));
    }
    let width_px = (width_pt * scale).round().max(1.0) as u32;

    let rendered = render_page_rgba(&auto.state, &key, page, width_px, true)?;
    let png = encode_png(rendered.width, rendered.height, &rendered.rgba)?;
    match &args.out {
        None => Ok(vec![Content::Png { data: png, width: rendered.width, height: rendered.height }]),
        Some(out) => {
            let target = paths::output_path(auto.root(), out)?;
            paths::write_atomic(&target, &png)?;
            json(json!({
                "path": target.to_string_lossy(),
                "width": rendered.width,
                "height": rendered.height,
                "bytes": png.len(),
            }))
        }
    }
}

fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, ToolError> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|e| failed(format!("PNG encoding failed: {e}")))?;
    Ok(out)
}
