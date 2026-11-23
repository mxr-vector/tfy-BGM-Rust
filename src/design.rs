//! 曲库、节目单与播放器共用的色彩和版式。

use std::time::{Duration, Instant};

use gpui::{AnyElement, ObjectFit, Rgba, div, img, prelude::*, px, rgb};

use crate::icons::icon;

#[derive(Clone, Copy)]
pub enum Color {
    Bg,
    Sidebar,
    Panel,
    Selected,
    Border,
    Text,
    Muted,
    Faint,
    Accent,
    AccentInk,
    Favorite,
    Danger,
    Control,
    AccentHover,
    Warning,
    Scrim,
}
pub use Color::{
    Accent as ACCENT, AccentHover as ACCENT_HOVER, AccentInk as ACCENT_INK, Bg as BG,
    Border as LINE, Control as CONTROL_TINT, Danger as DANGER, Faint as FAINT,
    Favorite as FAVORITE, Muted as MUTED, Panel as PANEL, Scrim as SCRIM, Selected as PANEL_LIGHT,
    Sidebar as SIDE, Text as TEXT, Warning as WARNING,
};

thread_local! {
    static WORKSPACE_DARK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn set_workspace_dark(dark: bool) {
    WORKSPACE_DARK.set(dark);
}

pub fn is_dark() -> bool {
    WORKSPACE_DARK.get()
}

pub fn theme_color(role: Color) -> Rgba {
    let (light, dark) = match role {
        Color::Bg => (0xfafafb, 0x161922),
        Color::Sidebar => (0xf1f3f5, 0x1b1e28),
        Color::Panel => (0xffffff, 0x212531),
        Color::Selected => (0xffe9ed, 0x3e2635),
        Color::Border => (0xe2e4e9, 0x373d4d),
        Color::Text => (0x222735, 0xf1f2f7),
        Color::Muted => (0x515b6d, 0xb7bfd1),
        Color::Faint => (0x657083, 0x9ba6bd),
        Color::Accent => (0xd92e4b, 0xff6e86),
        Color::AccentInk => (0xffffff, 0x20131a),
        Color::Favorite => (0xd92e4b, 0xff6e86),
        Color::Danger => (0xaa283b, 0xff8a9b),
        Color::Control => (0x25324b, 0xc3cde8),
        Color::AccentHover => (0xc32341, 0xff91a4),
        Color::Warning => (0x805416, 0xf3be75),
        Color::Scrim => (0x253047, 0x000000),
    };
    rgb(if is_dark() { dark } else { light })
}

pub const SIDEBAR_WIDTH: f32 = 204.;
pub const PAGE_TITLE: f32 = 29.;
pub const ROW_HEIGHT: f32 = 52.;
pub const PLAYER_HEIGHT: f32 = 84.;

pub const TRACK_PAGE_TRAVEL: f32 = 52.;
const TRACK_PAGE_DURATION: Duration = Duration::from_millis(260);

pub struct TrackTransition {
    started: Instant,
    duration: Duration,
    from: f32,
    target: f32,
}

impl TrackTransition {
    pub fn new(from: f32, open: bool, now: Instant) -> Option<Self> {
        let target = if open { 1. } else { 0. };
        let distance = (target - from).abs();
        (distance > f32::EPSILON).then(|| Self {
            started: now,
            duration: TRACK_PAGE_DURATION.mul_f32(distance.max(0.25)),
            from,
            target,
        })
    }

    pub fn position(&self, now: Instant) -> f32 {
        let time = now.saturating_duration_since(self.started).as_secs_f32();
        let elapsed = (time / self.duration.as_secs_f32()).min(1.);
        let eased = if self.target > self.from {
            1. - (1. - elapsed).powi(3)
        } else {
            elapsed * elapsed * (3. - 2. * elapsed)
        };
        self.from + (self.target - self.from) * eased
    }

    pub fn finished(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started) >= self.duration
    }
}

pub fn sleeve(
    category: &str,
    symbol: &'static str,
    tint: Rgba,
    icon_size: f32,
    radius: f32,
) -> AnyElement {
    if let Some(asset) = crate::assets::sleeve_path(category) {
        img(asset)
            .rounded(px(radius))
            .size_full()
            .object_fit(ObjectFit::Cover)
            .into_any_element()
    } else {
        div()
            .rounded(px(radius))
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(tint)
            .child(icon(symbol, icon_size, gpui::white()))
            .into_any_element()
    }
}

pub fn record(
    diameter: f32,
    category: &str,
    symbol: &'static str,
    tint: Rgba,
) -> impl IntoElement + use<> {
    let label = diameter * 0.44;
    div()
        .relative()
        .size(px(diameter))
        .flex_none()
        .rounded_full()
        .shadow_xl()
        .flex()
        .items_center()
        .justify_center()
        .child(
            gpui::canvas(
                |_, _, _| (),
                |bounds, _, window, _| {
                    use gpui::{Bounds, PathBuilder, point, quad, size};
                    let center = bounds.center();
                    let radius = bounds.size.width * 0.5;
                    let clear = gpui::transparent_black();
                    window.paint_quad(quad(
                        bounds,
                        radius,
                        rgb(0x14151a),
                        px(1.),
                        rgb(0x55545a),
                        Default::default(),
                    ));
                    for side in [0., std::f32::consts::PI] {
                        for wedge in 0..32 {
                            let start = side - 1.46 + wedge as f32 * 0.038;
                            let shine = (std::f32::consts::PI * (wedge as f32 + 0.5) / 32.)
                                .sin()
                                .powi(2);
                            let rim = radius - px(2.);
                            let mut path = PathBuilder::fill();
                            path.move_to(center);
                            for angle in [start, start + 0.039] {
                                path.line_to(point(
                                    center.x + rim * angle.cos(),
                                    center.y + rim * angle.sin(),
                                ));
                            }
                            path.close();
                            if let Ok(path) = path.build() {
                                window.paint_path(
                                    path,
                                    gpui::Hsla::from(rgb(0xb6bdc9)).opacity(shine * 0.19),
                                );
                            }
                        }
                    }
                    for i in 0..42 {
                        let r = radius * (0.48 + i as f32 * 0.012);
                        let ring =
                            Bounds::new(point(center.x - r, center.y - r), size(r * 2., r * 2.));
                        window.paint_quad(quad(
                            ring,
                            r,
                            clear,
                            px(if i % 7 == 0 { 1.1 } else { 0.65 }),
                            gpui::white().opacity(if i % 7 == 0 { 0.15 } else { 0.075 }),
                            Default::default(),
                        ));
                    }
                },
            )
            .absolute()
            .inset_0(),
        )
        .child(
            div()
                .relative()
                .size(px(label))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .child(sleeve(category, symbol, tint, label * 0.28, label / 2.))
                .child(
                    div()
                        .absolute()
                        .size(px(diameter * 0.025))
                        .rounded_full()
                        .bg(rgb(0x171921))
                        .border_1()
                        .border_color(gpui::white()),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn song_page_reverses_from_its_current_position() {
        let started = Instant::now();
        let opening = TrackTransition::new(0., true, started).unwrap();
        assert_eq!(opening.position(started), 0.);
        let midpoint = started + TRACK_PAGE_DURATION / 2;
        let visible = opening.position(midpoint);
        assert!((0.0..1.0).contains(&visible));

        let closing = TrackTransition::new(visible, false, midpoint).unwrap();
        assert_eq!(closing.position(midpoint), visible);
        let fading = closing.position(midpoint + closing.duration / 2);
        assert!((0.0..visible).contains(&fading));
        let end = midpoint + closing.duration;
        assert!(closing.finished(end));
        assert_eq!(closing.position(end), 0.);
        assert!(TrackTransition::new(0., false, end).is_none());
    }
}
