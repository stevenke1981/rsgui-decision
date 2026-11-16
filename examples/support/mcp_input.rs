//! Capture-bound pointer input and bounded Unicode text batches.
use super::{active, capture_region, screen_region};
use autogui::{AutoGui, Display, Error, Point, Rect, Result, Screenshot, Window};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub(super) struct CaptureProof {
    pub id: String,
    pub target: String,
    pub display: Option<String>,
    pub bounds: Rect,
    pub region: Rect,
    pub frame: Screenshot,
    pub taken: Instant,
}

impl CaptureProof {
    pub(super) fn fresh(&self) -> Result<()> {
        if self.taken.elapsed() >= Duration::from_secs(60) {
            return Err(Error::InvalidArgument("capture expired; capture again"));
        }
        Ok(())
    }

    pub fn point(&self, target: &str, id: &str, x: i32, y: i32) -> Result<Point> {
        if self.target != target || self.id != id {
            return Err(Error::InvalidArgument(
                "capture does not match target/id; capture again",
            ));
        }
        self.fresh()?;
        if !Rect::new(0, 0, self.region.width, self.region.height).contains(x, y) {
            return Err(Error::InvalidArgument(
                "point must be inside the captured image",
            ));
        }
        let x = self.region.left.checked_add(x);
        let y = self.region.top.checked_add(y);
        match (x, y) {
            (Some(x), Some(y)) if self.bounds.contains(x, y) => Ok(Point::new(x, y)),
            _ => Err(Error::InvalidArgument(
                "point outside captured window or coordinate overflow",
            )),
        }
    }

    pub fn patch(&self, point: Point) -> Result<(Rect, Screenshot)> {
        let x = point.x - self.region.left;
        let y = point.y - self.region.top;
        let left = (x - 16).max(0);
        let top = (y - 16).max(0);
        let right = (i64::from(x) + 16).min(i64::from(self.region.width)) as i32;
        let bottom = (i64::from(y) + 16).min(i64::from(self.region.height)) as i32;
        let local = Rect::new(left, top, right - left, bottom - top);
        let frame = self.frame.region(local)?;
        Ok((
            Rect::new(
                self.region.left + left,
                self.region.top + top,
                local.width,
                local.height,
            ),
            frame,
        ))
    }

    pub fn verify_geometry(
        &self,
        gui: &mut AutoGui,
        window: &Window,
        display: Option<&Display>,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        check()?;
        self.fresh()?;
        active(window)?;
        if window.box_rect()? != self.bounds || screen_region(gui, window, display)? != self.region
        {
            return Err(Error::InvalidArgument(
                "window/display geometry changed; capture again",
            ));
        }
        if let Some(display) = display {
            display.validate()?;
        }
        check()?;
        Ok(())
    }

    pub fn verify(
        &self,
        gui: &mut AutoGui,
        window: &Window,
        display: Option<&Display>,
        point: Point,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        self.verify_geometry(gui, window, display, check)?;
        let (region, expected) = self.patch(point)?;
        if capture_region(gui, display, region)? != expected {
            return Err(Error::InvalidArgument(
                "pixels or hover changed; cursor may be positioned; capture again before retrying",
            ));
        }
        self.verify_geometry(gui, window, display, check)?;
        Ok(())
    }
}

/// Checks whether `actual` matches `expected` cleanly. Exact match returns immediately; when
/// `allow_caret_drift` is true, permits tiny out-of-target pixel changes (e.g., blinking text caret
/// up to max(64, min(2500, total_pixels / 1000)) pixels) while strictly rejecting any change inside
/// candidate click/scroll 32x32 local patches.
pub(super) fn frames_match_with_tolerance(
    expected: &Screenshot,
    actual: &Screenshot,
    protected_local_points: &[Point],
    allow_caret_drift: bool,
) -> bool {
    if expected.width != actual.width
        || expected.height != actual.height
        || expected.pixels.len() != actual.pixels.len()
    {
        return false;
    }
    if expected.pixels == actual.pixels {
        return true;
    }
    if !allow_caret_drift || expected.width == 0 || expected.height == 0 {
        return false;
    }
    let width = expected.width as usize;
    let height = expected.height as usize;
    let row_bytes = width * 3;
    if expected.pixels.len() != height * row_bytes {
        return false;
    }
    for point in protected_local_points {
        let left = (point.x - 16).max(0) as usize;
        let top = (point.y - 16).max(0) as usize;
        let right = (i64::from(point.x) + 16).min(expected.width as i64).max(0) as usize;
        let bottom = (i64::from(point.y) + 16).min(expected.height as i64).max(0) as usize;
        if left >= right || top >= bottom {
            return false;
        }
        for row in top..bottom {
            let start = row * row_bytes + left * 3;
            let end = row * row_bytes + right * 3;
            if expected.pixels[start..end] != actual.pixels[start..end] {
                return false;
            }
        }
    }
    let total_pixels = width * height;
    let budget = (total_pixels / 1000).clamp(64, 2500);
    let mut changed = 0usize;
    for (a, b) in expected
        .pixels
        .as_chunks::<3>()
        .0
        .iter()
        .zip(actual.pixels.as_chunks::<3>().0.iter())
    {
        if a != b {
            changed += 1;
            if changed > budget {
                return false;
            }
        }
    }
    true
}

pub(super) fn validate_type_text(text: &str, min_chars: usize) -> Result<()> {
    let count = text.chars().count();
    if count < min_chars || count > 4096 {
        return Err(if min_chars == 1 {
            Error::InvalidArgument("text must contain 1..4096 characters")
        } else {
            Error::InvalidArgument("text exceeds 4096 characters; send smaller chunks")
        });
    }
    if text.contains('\0') {
        return Err(Error::InvalidArgument("text must not contain NUL bytes"));
    }
    Ok(())
}

pub(super) fn validate_hotkey(keys: &[String]) -> Result<()> {
    if keys.is_empty() || keys.len() > 8 || keys.iter().any(|key| key.is_empty() || key.len() > 32)
    {
        return Err(Error::InvalidArgument(
            "use 1..8 canonical keys, e.g. ctrl,a or enter",
        ));
    }
    if keys.iter().any(|key| autogui::normalize(key).is_none()) {
        return Err(Error::InvalidArgument(
            "unknown hotkey name; use canonical key names e.g. ctrl,a or enter",
        ));
    }
    Ok(())
}

pub(super) fn type_chunks(text: &str, mut send: impl FnMut(&str) -> Result<()>) -> Result<()> {
    let mut remaining = text;
    while !remaining.is_empty() {
        let end = remaining
            .char_indices()
            .nth(32)
            .map_or(remaining.len(), |(i, _)| i);
        send(&remaining[..end])?;
        remaining = &remaining[end..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use autogui::Rgb;

    fn proof() -> CaptureProof {
        CaptureProof {
            id: "c1".into(),
            target: "w1-0".into(),
            display: None,
            bounds: Rect::new(-100, 50, 100, 80),
            region: Rect::new(-100, 50, 100, 80),
            frame: Screenshot::solid(100, 80, Rgb::new(1, 2, 3)),
            taken: Instant::now(),
        }
    }

    #[test]
    fn capture_binding_expiry_and_half_open_coordinates() {
        let mut p = proof();
        assert_eq!(p.point("w1-0", "c1", 0, 0).unwrap(), Point::new(-100, 50));
        assert_eq!(p.point("w1-0", "c1", 99, 79).unwrap(), Point::new(-1, 129));
        for (target, id, x, y) in [
            ("w2-0", "c1", 1, 1),
            ("w1-0", "c0", 1, 1),
            ("w1-0", "c1", -1, 0),
            ("w1-0", "c1", 100, 0),
            ("w1-0", "c1", 0, 80),
        ] {
            assert!(matches!(
                p.point(target, id, x, y),
                Err(Error::InvalidArgument(_))
            ));
        }
        p.taken = Instant::now() - Duration::from_secs(61);
        assert!(matches!(
            p.point("w1-0", "c1", 1, 1),
            Err(Error::InvalidArgument(_))
        ));
        p.taken = Instant::now();
        p.region.left = i32::MAX - 1;
        assert!(matches!(
            p.point("w1-0", "c1", 2, 1),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn nonempty_edge_patches_and_changed_pixels() {
        let p = proof();
        for (x, y) in [(0, 0), (99, 0), (0, 79), (99, 79), (50, 40)] {
            let point = p.point("w1-0", "c1", x, y).unwrap();
            let (region, mut frame) = p.patch(point).unwrap();
            assert!(region.width > 0 && region.height > 0);
            assert!(region.width <= 32 && region.height <= 32);
            assert_eq!(region.intersect(&p.region), Some(region));
            frame.set_pixel(0, 0, Rgb::new(4, 5, 6)).unwrap();
            assert_ne!(p.patch(point).unwrap().1, frame);
        }
    }

    #[test]
    fn unicode_batches_and_stop_on_cancel_or_focus_error() {
        for count in [0, 31, 32, 33, 1000] {
            let input: String = "中🙂\n\té".chars().cycle().take(count).collect();
            let mut chunks = Vec::new();
            type_chunks(&input, |part| {
                chunks.push(part.to_string());
                Ok(())
            })
            .unwrap();
            assert_eq!(chunks.concat(), input);
            assert_eq!(chunks.len(), count.div_ceil(32));
            assert!(chunks.iter().all(|part| part.chars().count() <= 32));
        }
        let mut calls = 0;
        let error = type_chunks(&"x".repeat(1000), |_| {
            calls += 1;
            if calls == 2 {
                Err(Error::PermissionDenied("focus lost"))
            } else {
                Ok(())
            }
        });
        assert!(matches!(error, Err(Error::PermissionDenied(_))));
        assert_eq!(calls, 2);
    }

    #[test]
    fn text_and_hotkey_validation_rejects_nul_and_unknown_keys() {
        assert!(validate_type_text("", 0).is_ok());
        assert!(validate_type_text("", 1).is_err());
        assert!(validate_type_text("abc\0def", 0).is_err());
        assert!(validate_type_text("abc\0def", 1).is_err());
        assert!(validate_hotkey(&["ctrl".into(), "a".into()]).is_ok());
        assert!(validate_hotkey(&["insert".into()]).is_ok());
        assert!(validate_hotkey(&["ctrl".into(), "not_a_real_key".into()]).is_err());
        assert!(validate_hotkey(&[]).is_err());
    }

    #[test]
    fn caret_drift_tolerance_allows_tiny_distant_caret_but_blocks_target_or_large_changes() {
        let base = Screenshot::solid(200, 100, Rgb::new(10, 20, 30));
        let mut caret = base.clone();
        // Flip a 2x16 caret at top-left (32 px <= 64 budget), distant from click target (150, 50).
        for y in 0..16 {
            for x in 0..2 {
                let idx = (y * 200 + x) * 3;
                caret.pixels[idx] = 255;
            }
        }
        let target = Point::new(150, 50);
        assert!(!frames_match_with_tolerance(
            &base,
            &caret,
            &[target],
            false
        ));
        assert!(frames_match_with_tolerance(&base, &caret, &[target], true));

        // Flip inside the 32x32 protected patch around target (150, 50): must fail even with drift.
        let mut target_changed = base.clone();
        let idx = (50 * 200 + 150) * 3;
        target_changed.pixels[idx] = 255;
        assert!(!frames_match_with_tolerance(
            &base,
            &target_changed,
            &[target],
            true
        ));

        // Flip 80 pixels outside target (> 64 budget on 200x100 frame): must fail.
        let mut layout_shift = base.clone();
        for x in 0..80 {
            layout_shift.pixels[x * 3] = 255;
        }
        assert!(!frames_match_with_tolerance(
            &base,
            &layout_shift,
            &[target],
            true
        ));
    }
}
