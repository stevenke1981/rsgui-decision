//! Explicit Windows display capture, including monitors supplied by an IDD.
//! A Display is a topology snapshot. Re-enumerate after reconnecting, moving,
//! resizing or changing the primary monitor; stale snapshots fail closed.
use crate::{Error, Point, Rect, Result, Screenshot};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Display {
    id: u32,
    name: String,
    friendly_name: String,
    bounds: Rect,
    primary: bool,
}

impl Display {
    /// Enumerate active Windows displays. An installed but disabled driver or
    /// disconnected monitor does not constitute a capture target.
    pub fn all() -> Result<Vec<Self>> {
        #[cfg(all(windows, feature = "os"))]
        {
            let _dpi = crate::dpi::DpiGuard::enter()?;
            xcap::Monitor::all()
                .map_err(capture_error)?
                .iter()
                .map(Self::describe)
                .collect()
        }
        #[cfg(not(all(windows, feature = "os")))]
        {
            Err(unavailable())
        }
    }

    pub fn id(&self) -> u32 {
        self.id
    }
    /// Windows device name, such as \\.\DISPLAY2. Select exact names, not indices.
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn friendly_name(&self) -> &str {
        &self.friendly_name
    }
    /// Physical pixels in the Windows virtual desktop; left/top may be negative.
    pub fn bounds(&self) -> Rect {
        self.bounds
    }
    pub fn is_primary(&self) -> bool {
        self.primary
    }

    pub fn contains(&self, point: Point) -> bool {
        i64::from(point.x) >= i64::from(self.bounds.left)
            && i64::from(point.y) >= i64::from(self.bounds.top)
            && i64::from(point.x) < i64::from(self.bounds.left) + i64::from(self.bounds.width)
            && i64::from(point.y) < i64::from(self.bounds.top) + i64::from(self.bounds.height)
    }

    /// Reject disconnected displays and topology changes instead of capturing a
    /// different display that happens to reuse an identifier.
    pub fn validate(&self) -> Result<()> {
        self.validate_with_virtual_bounds().map(|_| ())
    }

    pub(crate) fn validate_with_virtual_bounds(&self) -> Result<Rect> {
        #[cfg(all(windows, feature = "os"))]
        {
            let _dpi = crate::dpi::DpiGuard::enter()?;
            self.resolve_with_virtual_bounds().map(|(_, bounds)| bounds)
        }
        #[cfg(not(all(windows, feature = "os")))]
        {
            Err(unavailable())
        }
    }

    pub(crate) fn capture(&self, region: Rect) -> Result<Screenshot> {
        if region.width <= 0 || region.height <= 0 || region.intersect(&self.bounds) != Some(region)
        {
            return Err(Error::InvalidArgument(
                "capture region must fit inside the selected display",
            ));
        }
        #[cfg(all(windows, feature = "os"))]
        {
            let _dpi = crate::dpi::DpiGuard::enter()?;
            let monitor = self.resolve()?;
            let x = u32::try_from(i64::from(region.left) - i64::from(self.bounds.left))
                .map_err(|_| Error::InvalidArgument("display coordinate overflow"))?;
            let y = u32::try_from(i64::from(region.top) - i64::from(self.bounds.top))
                .map_err(|_| Error::InvalidArgument("display coordinate overflow"))?;
            let rgba = monitor
                .capture_region(x, y, region.width as u32, region.height as u32)
                .map_err(capture_error)?;
            if rgba.width() != region.width as u32 || rgba.height() != region.height as u32 {
                return Err(Error::Screenshot(
                    "display changed during capture; enumerate again".into(),
                ));
            }
            crate::backend::rgba_to_rgb(rgba.width(), rgba.height(), rgba.as_raw())
        }
        #[cfg(not(all(windows, feature = "os")))]
        {
            Err(unavailable())
        }
    }

    #[cfg(all(windows, feature = "os"))]
    fn monitor_bounds(monitor: &xcap::Monitor) -> Result<Rect> {
        let width = i32::try_from(monitor.width().map_err(capture_error)?)
            .map_err(|_| Error::InvalidArgument("display width overflow"))?;
        let height = i32::try_from(monitor.height().map_err(capture_error)?)
            .map_err(|_| Error::InvalidArgument("display height overflow"))?;
        if width <= 0 || height <= 0 {
            return Err(Error::Screenshot("display has no active mode".into()));
        }
        Ok(Rect::new(
            monitor.x().map_err(capture_error)?,
            monitor.y().map_err(capture_error)?,
            width,
            height,
        ))
    }

    #[cfg(all(windows, feature = "os"))]
    fn describe_identity(monitor: &xcap::Monitor) -> Result<(u32, String, Rect, bool)> {
        let id = monitor.id().map_err(capture_error)?;
        let name = monitor.name().map_err(capture_error)?;
        let bounds = Self::monitor_bounds(monitor)?;
        let primary = monitor.is_primary().map_err(capture_error)?;
        Ok((id, name, bounds, primary))
    }

    #[cfg(all(windows, feature = "os"))]
    fn describe(monitor: &xcap::Monitor) -> Result<Self> {
        let (id, name, bounds, primary) = Self::describe_identity(monitor)?;
        // Friendly labels are optional metadata; device identity never falls back.
        let friendly_name = monitor.friendly_name().unwrap_or_else(|_| name.clone());
        Ok(Self {
            id,
            name,
            friendly_name,
            bounds,
            primary,
        })
    }

    #[cfg(all(windows, feature = "os"))]
    fn resolve(&self) -> Result<xcap::Monitor> {
        for monitor in xcap::Monitor::all().map_err(capture_error)? {
            let (id, name, bounds, primary) = Self::describe_identity(&monitor)?;
            if id == self.id
                && name == self.name
                && bounds == self.bounds
                && primary == self.primary
            {
                return Ok(monitor);
            }
        }
        Err(Error::Screenshot(
            "selected display disconnected or changed; enumerate displays again".into(),
        ))
    }

    #[cfg(all(windows, feature = "os"))]
    fn resolve_with_virtual_bounds(&self) -> Result<(xcap::Monitor, Rect)> {
        let mut matched = None;
        let mut left = i64::MAX;
        let mut top = i64::MAX;
        let mut right = i64::MIN;
        let mut bottom = i64::MIN;
        for monitor in xcap::Monitor::all().map_err(capture_error)? {
            let (id, name, bounds, primary) = Self::describe_identity(&monitor)?;
            left = left.min(i64::from(bounds.left));
            top = top.min(i64::from(bounds.top));
            right = right.max(i64::from(bounds.left) + i64::from(bounds.width));
            bottom = bottom.max(i64::from(bounds.top) + i64::from(bounds.height));
            if matched.is_none()
                && id == self.id
                && name == self.name
                && bounds == self.bounds
                && primary == self.primary
            {
                matched = Some(monitor);
            }
        }
        let monitor = matched.ok_or_else(|| {
            Error::Screenshot(
                "selected display disconnected or changed; enumerate displays again".into(),
            )
        })?;
        Ok((monitor, virtual_rect(left, top, right, bottom)?))
    }
}

#[cfg(all(windows, feature = "os"))]
fn virtual_rect(left: i64, top: i64, right: i64, bottom: i64) -> Result<Rect> {
    let origin_x = i32::try_from(left)
        .map_err(|_| Error::InvalidArgument("virtual desktop origin overflow"))?;
    let origin_y = i32::try_from(top)
        .map_err(|_| Error::InvalidArgument("virtual desktop origin overflow"))?;
    let width = i32::try_from(right - left)
        .map_err(|_| Error::InvalidArgument("virtual desktop extent overflow"))?;
    let height = i32::try_from(bottom - top)
        .map_err(|_| Error::InvalidArgument("virtual desktop extent overflow"))?;
    if width <= 0 || height <= 0 {
        return Err(Error::InvalidArgument(
            "point is outside the virtual desktop",
        ));
    }
    Ok(Rect::new(origin_x, origin_y, width, height))
}

#[cfg(any(all(windows, feature = "os"), test))]
pub(crate) fn normalize_virtual_axis(
    value: i32,
    origin: i64,
    extent: i64,
    clamp_out_of_bounds: bool,
) -> Result<i32> {
    if extent <= 0 {
        return Err(Error::InvalidArgument(
            "virtual desktop extent must be positive",
        ));
    }
    let relative = i64::from(value) - origin;
    if !clamp_out_of_bounds && (relative < 0 || relative >= extent) {
        return Err(Error::InvalidArgument(
            "point is outside the virtual desktop",
        ));
    }
    if extent == 1 {
        return Ok(32768);
    }
    let clamped = relative.clamp(0, extent - 1);
    // Aim at the center of the requested pixel's normalized interval.
    // Exact cursor readback remains mandatory before sending a click.
    Ok(((clamped * 65536 + 32768) / extent).min(65535) as i32)
}

#[cfg(all(windows, feature = "os"))]
pub(crate) fn virtual_desktop_bounds() -> Result<Rect> {
    let mut left = i64::MAX;
    let mut top = i64::MAX;
    let mut right = i64::MIN;
    let mut bottom = i64::MIN;
    let mut found = false;
    for monitor in xcap::Monitor::all().map_err(capture_error)? {
        let bounds = Display::monitor_bounds(&monitor)?;
        left = left.min(i64::from(bounds.left));
        top = top.min(i64::from(bounds.top));
        right = right.max(i64::from(bounds.left) + i64::from(bounds.width));
        bottom = bottom.max(i64::from(bounds.top) + i64::from(bounds.height));
        found = true;
    }
    if !found {
        return Err(Error::Screenshot("no monitor found".into()));
    }
    virtual_rect(left, top, right, bottom)
}

#[cfg(all(windows, feature = "os"))]
pub(crate) fn send_mouse_abs(
    point: Point,
    virtual_bounds: Rect,
    require_inside: bool,
) -> Result<()> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_MOVE,
        MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT, SendInput,
    };
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: normalize_virtual_axis(
                    point.x,
                    i64::from(virtual_bounds.left),
                    i64::from(virtual_bounds.width),
                    !require_inside,
                )?,
                dy: normalize_virtual_axis(
                    point.y,
                    i64::from(virtual_bounds.top),
                    i64::from(virtual_bounds.height),
                    !require_inside,
                )?,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    if unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) } != 1 {
        return Err(Error::Input(format!(
            "virtual desktop SendInput failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

#[cfg(not(all(windows, feature = "os")))]
fn unavailable() -> Error {
    if cfg!(windows) {
        Error::FeatureDisabled("os")
    } else {
        Error::UnsupportedPlatform
    }
}

#[cfg(all(windows, feature = "os"))]
fn capture_error(error: xcap::XCapError) -> Error {
    Error::Screenshot(error.to_string())
}

#[cfg(all(windows, feature = "os"))]
pub(crate) fn move_cursor(point: Point) -> Result<()> {
    let _dpi = crate::dpi::DpiGuard::enter()?;
    // Use the same physical-pixel bounds as capture, including negative origins.
    // GetSystemMetrics is not DPI-aware under a per-monitor-aware thread.
    let mut left = i64::MAX;
    let mut top = i64::MAX;
    let mut right = i64::MIN;
    let mut bottom = i64::MIN;
    let mut contains_point = false;
    for monitor in xcap::Monitor::all().map_err(capture_error)? {
        let bounds = Display::monitor_bounds(&monitor)?;
        if i64::from(point.x) >= i64::from(bounds.left)
            && i64::from(point.y) >= i64::from(bounds.top)
            && i64::from(point.x) < i64::from(bounds.left) + i64::from(bounds.width)
            && i64::from(point.y) < i64::from(bounds.top) + i64::from(bounds.height)
        {
            contains_point = true;
        }
        left = left.min(i64::from(bounds.left));
        top = top.min(i64::from(bounds.top));
        right = right.max(i64::from(bounds.left) + i64::from(bounds.width));
        bottom = bottom.max(i64::from(bounds.top) + i64::from(bounds.height));
    }
    if !contains_point {
        return Err(Error::InvalidArgument("point is outside active displays"));
    }
    send_mouse_abs(point, virtual_rect(left, top, right, bottom)?, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn display() -> Display {
        Display {
            id: 1,
            name: "test".into(),
            friendly_name: "test".into(),
            bounds: Rect::new(-1920, -1080, 1920, 1080),
            primary: false,
        }
    }
    #[test]
    fn negative_bounds_include_first_pixel_and_exclude_end() {
        let display = display();
        assert!(display.contains(Point::new(-1920, -1080)));
        assert!(display.contains(Point::new(-1, -1)));
        for point in [
            Point::new(-1921, -1),
            Point::new(-1, -1081),
            Point::new(0, -1),
            Point::new(-1, 0),
        ] {
            assert!(!display.contains(point));
        }
    }
    #[test]
    fn invalid_region_is_rejected_before_native_capture() {
        for region in [
            Rect::new(-1921, -1080, 2, 2),
            Rect::new(-1, -1, 2, 2),
            Rect::new(-1, -1, 0, 1),
        ] {
            assert!(matches!(
                display().capture(region),
                Err(Error::InvalidArgument(_))
            ));
        }
    }
    #[test]
    fn virtual_axis_normalization_preserves_primary_and_guards_small_extents() {
        // Extent <= 0 rejects; extent == 1 centers safely without dividing by zero.
        assert!(normalize_virtual_axis(0, 0, 0, true).is_err());
        assert_eq!(normalize_virtual_axis(0, 0, 1, false).unwrap(), 32768);
        assert_eq!(normalize_virtual_axis(5, 0, 1, true).unwrap(), 32768);
        assert!(normalize_virtual_axis(5, 0, 1, false).is_err());

        // Single primary monitor 1920x1080 at (0, 0): inverse (dx * 1920) >> 16 recovers x.
        for x in [0, 1, 500, 960, 1919] {
            let dx = normalize_virtual_axis(x, 0, 1920, false).unwrap();
            assert_eq!((i64::from(dx) * 1920) >> 16, i64::from(x));
        }

        // Two horizontal monitors [-1920..0) + [0..1920): primary x=100 stays primary x=100.
        let dx_primary = normalize_virtual_axis(100, -1920, 3840, false).unwrap();
        assert_eq!(((i64::from(dx_primary) * 3840) >> 16) - 1920, 100);
        let dx_left = normalize_virtual_axis(-1920, -1920, 3840, false).unwrap();
        assert_eq!(((i64::from(dx_left) * 3840) >> 16) - 1920, -1920);

        // Unclamped mode rejects out-of-bounds; clamped mode pins edges for tween overshoot.
        assert!(normalize_virtual_axis(-1, 0, 1920, false).is_err());
        assert_eq!(
            normalize_virtual_axis(-5, 0, 1920, true).unwrap(),
            normalize_virtual_axis(0, 0, 1920, true).unwrap()
        );
        assert_eq!(
            normalize_virtual_axis(1925, 0, 1920, true).unwrap(),
            normalize_virtual_axis(1919, 0, 1920, true).unwrap()
        );
    }
}
