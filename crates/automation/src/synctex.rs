//! SyncTeX tools: forward search (a source line to its boxes on the pages) and inverse search
//! (a point on a page to its source line), from the SyncTeX file TeX wrote beside the open PDF.
//! Points are from the top-left of the displayed page, as everywhere in these tools.

use std::path::Path;

use pdfcraft_engine::synctex::{self, Synctex};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, failed};

impl Automation {
    /// The SyncTeX map of the document's file. Only the file beside the PDF is read, which is
    /// inside the root whenever the PDF is.
    fn synctex_map(&self, a: &Args) -> Result<Synctex> {
        let doc = self.doc(a)?;
        let path = doc.path.as_deref().ok_or_else(|| failed("the document has no file, so it has no SyncTeX file"))?;
        synctex::load_for(Path::new(path))
            .map_err(failed)?
            .ok_or_else(|| failed(format!("no SyncTeX file next to {}; build it with -synctex=1", doc.display_name())))
    }

    pub(crate) fn synctex_forward(&self, a: &Args) -> Result<Value> {
        let line =
            u32::try_from(a.int("line")?).ok().filter(|line| *line >= 1).ok_or_else(|| ToolError::InvalidArgs("line must be 1 or more".into()))?;
        let column = match a.opt_int("column")? {
            Some(column) => {
                Some(u32::try_from(column).ok().filter(|c| *c >= 1).ok_or_else(|| ToolError::InvalidArgs("column must be 1 or more".into()))?)
            }
            None => None,
        };
        let source = a.str("source")?;
        let map = self.synctex_map(a)?;
        let hit = map.forward(Path::new(source), line, column).ok_or_else(|| failed(format!("the SyncTeX file has no box for {source}:{line}")))?;
        let page =
            self.doc(a)?.info.pages.get(hit.page).ok_or_else(|| failed(format!("SyncTeX names page {}, which the document lacks", hit.page + 1)))?;
        let boxes: Vec<Value> = hit
            .boxes
            .iter()
            .map(|&[x0, y0, x1, y1]| {
                let [ux0, uy0] = synctex::page_to_user(page, x0, y0);
                let [ux1, uy1] = synctex::page_to_user(page, x1, y1);
                let ([vx0, vy0], [vx1, vy1]) = (page.user_to_view(ux0, uy0), page.user_to_view(ux1, uy1));
                json!([vx0.min(vx1), vy0.min(vy1), vx0.max(vx1), vy0.max(vy1)])
            })
            .collect();
        Ok(json!({ "page": hit.page + 1, "line": hit.line, "boxes": boxes }))
    }

    pub(crate) fn synctex_inverse(&self, a: &Args) -> Result<Value> {
        let index = self.page(a)?;
        let number = |key: &str| a.opt_num(key)?.ok_or_else(|| ToolError::InvalidArgs(format!("{key} is required")));
        let (x, y) = (number("x")?, number("y")?);
        let map = self.synctex_map(a)?;
        let page = self.doc(a)?.info.pages.get(index).ok_or_else(|| failed("no such page"))?;
        let [ux, uy] = page.view_to_user(x as f32, y as f32);
        let [px, py] = synctex::user_to_page(page, ux, uy);
        let place = map.inverse(index, px, py).ok_or_else(|| failed(format!("the SyncTeX file has no source line for page {}", index + 1)))?;
        Ok(json!({ "file": place.file.to_string_lossy(), "line": place.line, "column": place.column }))
    }
}
