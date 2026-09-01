//! Conversion between the coordinate space the *frontend* speaks and PDF user
//! space, for pages that may carry a `/Rotate` (issue #121).
//!
//! Two spaces meet here:
//!
//! - **Render space** — what the overlays measure in. PDF points, **top-left**
//!   origin, y growing downward, measured against the page *as pdfium drew it*.
//!   pdfium renders the CropBox and it renders it rotated, so for `/Rotate 90`
//!   and `270` the rendered width and height are swapped relative to user
//!   space. Search rectangles, redaction rectangles, typewriter note boxes and
//!   ink stroke points are all in this space.
//! - **User space** — what goes in the file. PDF points, bottom-left origin, y
//!   growing upward, and *un*rotated: a viewer applies `/Rotate` on the way to
//!   the screen, so page content and annotation `/Rect`s are authored as though
//!   the page were never turned.
//!
//! Getting the mapping wrong fails **invisibly**: the content is written into
//! the file at coordinates nobody can see. Every consumer here is therefore
//! covered by a test that renders the page and asserts *where* the pixels
//! landed, not merely that some were emitted.
//!
//! The rotation convention is `/Rotate 90` = displayed 90° **clockwise**, so
//! the user-space bottom-left corner lands at the display's top-left. That is
//! pdfium's behaviour, pinned by [`crate::commands::margins`]'s rotation
//! tests, and this module's mapping agrees with `margins::user_to_display` on
//! all four rotations.

use crate::commands::typewriter::object_as_f32;
use lopdf::{Document, Object, ObjectId};

/// A 2×3 affine matrix in PDF order: `[a, b, c, d, e, f]`.
pub(crate) type Mat = [f32; 6];

/// A page's (possibly inherited) `/Rotate`, defaulting to 0.
///
/// `/Rotate` is an inheritable page-tree attribute, so a rotation set once on
/// the `/Pages` node applies to every page under it and reading only the page
/// dictionary would report 0 for a whole rotated document.
pub(crate) fn inherited_rotate(doc: &Document, page_id: ObjectId) -> i64 {
    let mut current = page_id;
    for _ in 0..64 {
        let Some(dict) = doc.get_object(current).ok().and_then(|o| o.as_dict().ok()) else {
            return 0;
        };
        if let Ok(Object::Integer(r)) = dict.get(b"Rotate") {
            return *r;
        }
        match dict.get(b"Parent").and_then(|p| p.as_reference()) {
            Ok(parent) => current = parent,
            Err(_) => return 0,
        }
    }
    0
}

/// Resolves a page's (possibly inherited) rect entry — `/MediaBox` or
/// `/CropBox` — normalized so `x0 < x1`, `y0 < y1`. `None` if unreadable.
pub(crate) fn inherited_rect(doc: &Document, page_id: ObjectId, key: &[u8]) -> Option<[f32; 4]> {
    let mut current = page_id;
    for _ in 0..64 {
        let dict = doc.get_object(current).ok()?.as_dict().ok()?;
        if let Ok(value) = dict.get(key) {
            let arr = match value {
                Object::Reference(r) => doc.get_object(*r).ok()?.as_array().ok()?,
                Object::Array(a) => a,
                _ => return None,
            };
            if arr.len() >= 4 {
                let v: Vec<f32> = arr.iter().map(object_as_f32).collect();
                return Some([
                    v[0].min(v[2]),
                    v[1].min(v[3]),
                    v[0].max(v[2]),
                    v[1].max(v[3]),
                ]);
            }
        }
        current = dict.get(b"Parent").ok()?.as_reference().ok()?;
    }
    None
}

/// The geometry a page presents to the frontend: the box pdfium rendered, plus
/// the rotation it rendered it at. Everything converting overlay coordinates
/// into user space (or back) should go through one of these.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PageSpace {
    /// Normalized to 0, 90, 180 or 270.
    rotate: i64,
    /// The render box in user space, `[x0, y0, x1, y1]`, normalized.
    ebox: [f32; 4],
}

impl PageSpace {
    /// Reads a page's render space: `CropBox` when present (that is the box
    /// pdfium draws, so a cropped page is displayed cropped and the overlay
    /// coordinates are relative to the crop), else `MediaBox`, else US Letter.
    pub(crate) fn of(doc: &Document, page_id: ObjectId) -> Self {
        let ebox = inherited_rect(doc, page_id, b"CropBox")
            .or_else(|| inherited_rect(doc, page_id, b"MediaBox"))
            .unwrap_or([0.0, 0.0, 612.0, 792.0]);
        Self::new(inherited_rotate(doc, page_id), ebox)
    }

    /// Builds a space directly, normalizing the rotation to 0/90/180/270. A
    /// `/Rotate` that is not a multiple of 90 is invalid per the spec; it is
    /// rounded to the nearest quarter turn rather than rejected, since a
    /// malformed page should still place content roughly right.
    pub(crate) fn new(rotate: i64, ebox: [f32; 4]) -> Self {
        let quarter = (rotate as f64 / 90.0).round() as i64;
        Self {
            rotate: quarter.rem_euclid(4) * 90,
            ebox: [
                ebox[0].min(ebox[2]),
                ebox[1].min(ebox[3]),
                ebox[0].max(ebox[2]),
                ebox[1].max(ebox[3]),
            ],
        }
    }

    /// The render box's width and height **in user space** — not swapped for a
    /// rotated page. See [`Self::render_size`] for what the viewer showed.
    fn size(&self) -> (f32, f32) {
        (self.ebox[2] - self.ebox[0], self.ebox[3] - self.ebox[1])
    }

    /// The normalized rotation, and the page's size as the viewer showed it —
    /// width and height exchanged at 90 and 270.
    ///
    /// Both are `cfg(test)`: production code goes through the conversions
    /// below, which is the point of having them. These describe the same
    /// geometry from the frontend's side, so the tests can state their
    /// expectations in the terms the user experiences rather than
    /// re-deriving the swap they are checking for.
    #[cfg(test)]
    fn rotate(&self) -> i64 {
        self.rotate
    }

    #[cfg(test)]
    fn render_size(&self) -> (f32, f32) {
        let (w, h) = self.size();
        if self.rotate == 90 || self.rotate == 270 {
            (h, w)
        } else {
            (w, h)
        }
    }

    /// Render space (top-left origin) → user space (bottom-left origin).
    pub(crate) fn to_user(&self, [fx, fy]: [f32; 2]) -> [f32; 2] {
        let [x0, y0, _, _] = self.ebox;
        let (w, h) = self.size();
        match self.rotate {
            90 => [x0 + fy, y0 + fx],
            180 => [x0 + w - fx, y0 + fy],
            270 => [x0 + w - fy, y0 + h - fx],
            _ => [x0 + fx, y0 + h - fy],
        }
    }

    /// User space → render space. The inverse of [`Self::to_user`].
    pub(crate) fn to_render(&self, [ux, uy]: [f32; 2]) -> [f32; 2] {
        let [x0, y0, _, _] = self.ebox;
        let (w, h) = self.size();
        match self.rotate {
            90 => [uy - y0, ux - x0],
            180 => [x0 + w - ux, uy - y0],
            270 => [y0 + h - uy, x0 + w - ux],
            _ => [ux - x0, y0 + h - uy],
        }
    }

    /// A render-space box `(x, y, width, height)` as a user-space rectangle
    /// `[x1, y1, x2, y2]` with `x1 < x2`, `y1 < y2`.
    ///
    /// Mapping both corners and taking the extremes is what makes the width /
    /// height exchange at 90 and 270 fall out on its own — a rotated note's
    /// user-space rect is as tall as the box is wide, and hand-writing that
    /// per rotation is exactly where the sign errors live.
    pub(crate) fn rect_to_user(&self, x: f32, y: f32, width: f32, height: f32) -> [f32; 4] {
        let a = self.to_user([x, y]);
        let b = self.to_user([x + width, y + height]);
        [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[0].max(b[0]),
            a[1].max(b[1]),
        ]
    }

    /// A user-space rectangle `[x1, y1, x2, y2]` back as a render-space box
    /// `(x, y, width, height)`, top-left origin.
    pub(crate) fn rect_to_render(&self, [x1, y1, x2, y2]: [f32; 4]) -> (f32, f32, f32, f32) {
        let a = self.to_render([x1, y1]);
        let b = self.to_render([x2, y2]);
        let (x, y) = (a[0].min(b[0]), a[1].min(b[1]));
        ((x), (y), (a[0] - b[0]).abs(), (a[1] - b[1]).abs())
    }

    /// The linear part of a transform that makes upright render-space content
    /// draw upright *on screen* once the viewer applies `/Rotate`.
    ///
    /// The viewer turns user space clockwise by the rotation, so content must
    /// be authored turned counter-clockwise by the same amount to come back
    /// level. Used both as an appearance stream's `/Matrix` and as the linear
    /// part of a text matrix; `e`/`f` are left at zero for the caller to fill
    /// in with the placement point.
    pub(crate) fn upright_matrix(&self) -> Mat {
        match self.rotate {
            90 => [0.0, 1.0, -1.0, 0.0, 0.0, 0.0],
            180 => [-1.0, 0.0, 0.0, -1.0, 0.0, 0.0],
            270 => [0.0, -1.0, 1.0, 0.0, 0.0, 0.0],
            _ => [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deliberately non-square and off-origin: on a square box the 90/270
    /// width/height exchange cancels and a wrong mapping passes silently, and
    /// a zero origin hides every missing offset.
    const EBOX: [f32; 4] = [10.0, 20.0, 210.0, 420.0]; // 200 x 400

    #[test]
    fn render_size_swaps_only_at_ninety_and_two_seventy() {
        let sizes: Vec<(f32, f32)> = [0, 90, 180, 270]
            .iter()
            .map(|r| PageSpace::new(*r, EBOX).render_size())
            .collect();
        assert_eq!(
            sizes,
            vec![(200.0, 400.0), (400.0, 200.0), (200.0, 400.0), (400.0, 200.0)]
        );
    }

    /// The render-space origin is the top-left of what the user saw. Which
    /// user-space corner that is depends entirely on the rotation, and getting
    /// it wrong puts content in the opposite corner of the page.
    #[test]
    fn render_origin_maps_to_the_corner_the_rotation_brought_to_the_top_left() {
        let corner = |r: i64| PageSpace::new(r, EBOX).to_user([0.0, 0.0]);
        assert_eq!(corner(0), [10.0, 420.0], "unrotated: top-left");
        assert_eq!(corner(90), [10.0, 20.0], "90° cw brings bottom-left up");
        assert_eq!(corner(180), [210.0, 20.0], "180° brings bottom-right up");
        assert_eq!(corner(270), [210.0, 420.0], "270° cw brings top-right over");
    }

    #[test]
    fn point_round_trips_through_user_space_at_every_rotation() {
        for rotate in [0, 90, 180, 270] {
            let space = PageSpace::new(rotate, EBOX);
            let (rw, rh) = space.render_size();
            for p in [[0.0, 0.0], [rw, rh], [17.0, 133.0], [rw / 3.0, rh / 7.0]] {
                let back = space.to_render(space.to_user(p));
                assert!(
                    (back[0] - p[0]).abs() < 1e-3 && (back[1] - p[1]).abs() < 1e-3,
                    "rotate {rotate}: {p:?} round-tripped to {back:?}"
                );
            }
        }
    }

    /// Every render-space point must land inside the box — a mapping that is
    /// self-consistent but shifted by the origin round-trips perfectly while
    /// placing everything off the visible page.
    #[test]
    fn mapped_points_stay_within_the_page_box() {
        for rotate in [0, 90, 180, 270] {
            let space = PageSpace::new(rotate, EBOX);
            let (rw, rh) = space.render_size();
            for p in [[0.0, 0.0], [rw, 0.0], [0.0, rh], [rw, rh]] {
                let [ux, uy] = space.to_user(p);
                assert!(
                    (EBOX[0]..=EBOX[2]).contains(&ux) && (EBOX[1]..=EBOX[3]).contains(&uy),
                    "rotate {rotate}: {p:?} → ({ux}, {uy}) is outside {EBOX:?}"
                );
            }
        }
    }

    /// A wide, short note box becomes a tall, narrow user-space rect on a
    /// quarter-turned page. This is the swap the issue is about.
    #[test]
    fn a_rect_exchanges_width_and_height_at_ninety_and_two_seventy() {
        let dims = |r: i64| {
            let [x1, y1, x2, y2] = PageSpace::new(r, EBOX).rect_to_user(5.0, 7.0, 120.0, 40.0);
            (x2 - x1, y2 - y1)
        };
        assert_eq!(dims(0), (120.0, 40.0));
        assert_eq!(dims(180), (120.0, 40.0));
        assert_eq!(dims(90), (40.0, 120.0), "quarter turn must swap");
        assert_eq!(dims(270), (40.0, 120.0), "quarter turn must swap");
    }

    #[test]
    fn rect_round_trips_through_user_space_at_every_rotation() {
        for rotate in [0, 90, 180, 270] {
            let space = PageSpace::new(rotate, EBOX);
            let want = (5.0, 7.0, 120.0, 40.0);
            let got = space.rect_to_render(space.rect_to_user(want.0, want.1, want.2, want.3));
            let close = |a: f32, b: f32| (a - b).abs() < 1e-3;
            assert!(
                close(got.0, want.0) && close(got.1, want.1) && close(got.2, want.2) && close(got.3, want.3),
                "rotate {rotate}: {want:?} round-tripped to {got:?}"
            );
        }
    }

    /// Not a multiple of 90 is invalid per the spec; content should still land
    /// roughly right rather than silently fall back to unrotated.
    #[test]
    fn rotation_is_normalized() {
        assert_eq!(PageSpace::new(-90, EBOX).rotate(), 270);
        assert_eq!(PageSpace::new(450, EBOX).rotate(), 90);
        assert_eq!(PageSpace::new(88, EBOX).rotate(), 90);
        assert_eq!(PageSpace::new(0, EBOX).rotate(), 0);
    }

    /// The upright matrix must undo the viewer's clockwise turn, so composing
    /// the two is the identity: a vector pointing along the text baseline ends
    /// up pointing along the screen's x axis.
    #[test]
    fn upright_matrix_cancels_the_pages_rotation() {
        for rotate in [0, 90, 180, 270] {
            let space = PageSpace::new(rotate, EBOX);
            let [a, b, c, d, _, _] = space.upright_matrix();
            // The baseline direction (1,0) in form space, as a user-space
            // vector, then through to render space. Render space flips y, so
            // an upright baseline points along +x with no y component.
            let (vx, vy) = (a, b);
            let origin = space.to_render([0.0, 0.0]);
            let tip = space.to_render([vx, vy]);
            assert!(
                (tip[0] - origin[0] - 1.0).abs() < 1e-3 && (tip[1] - origin[1]).abs() < 1e-3,
                "rotate {rotate}: baseline {:?} is not level on screen",
                [a, b, c, d]
            );
        }
    }

    #[test]
    fn crop_box_wins_over_media_box_and_rotate_is_inherited() {
        use lopdf::{dictionary, Stream, Dictionary};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let content = doc.add_object(Object::Stream(Stream::new(Dictionary::new(), Vec::new())));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "CropBox" => Object::Array(vec![
                Object::Real(10.0), Object::Real(20.0),
                Object::Real(210.0), Object::Real(420.0),
            ]),
            "Contents" => Object::Reference(content),
        });
        // MediaBox and Rotate live on the parent — both are inheritable, and
        // reading only the page dictionary would miss them.
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => Object::Array(vec![Object::Reference(page_id)]),
                "Count" => Object::Integer(1),
                "MediaBox" => Object::Array(vec![
                    Object::Real(0.0), Object::Real(0.0),
                    Object::Real(600.0), Object::Real(800.0),
                ]),
                "Rotate" => Object::Integer(90),
            }),
        );

        let space = PageSpace::of(&doc, page_id);
        assert_eq!(space.rotate(), 90, "inherited /Rotate missed");
        assert_eq!(space.size(), (200.0, 400.0), "CropBox must win over MediaBox");
        assert_eq!(space.render_size(), (400.0, 200.0));
    }
}
