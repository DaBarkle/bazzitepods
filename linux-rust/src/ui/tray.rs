//! System tray (StatusNotifierItem via ksni). Reads only from the hub.
//!
//! Icon: the themed `io.github.dabarkle.Kase-symbolic` icon (recoloured by the panel), plus a
//! small rendered battery badge as the overlay pixmap while connected. A fully rendered pixmap is
//! provided as a fallback for hosts that cannot resolve the icon name. Rendered icons are cached
//! by (level bucket, tone, charging, connected).

use crate::bluetooth::aacp::ControlCommandIdentifiers;
use crate::devices::airpods::CommandSender;
use crate::hub::{AppSnapshot, DeviceSnapshot, Hub};
use crate::ui::format::{self, ListeningMode, Tone};
use crate::ui::messages::BluetoothUIMessage;
use ksni::{Icon, ToolTip, TrayMethods};
use log::debug;
use std::collections::HashMap;
use std::f32::consts::TAU;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc::UnboundedSender;

const ICON_NAME: &str = "io.github.dabarkle.Kase-symbolic";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IconKey {
    /// Level rounded to 5 %, `None` when unknown.
    pub bucket: Option<u8>,
    pub tone: Tone,
    pub charging: bool,
    pub connected: bool,
}

impl IconKey {
    pub fn for_snapshot(snap: &AppSnapshot) -> Self {
        match snap.primary().filter(|d| d.is_connected()) {
            Some(d) => {
                let (level, charging) = format::tray_level(d);
                IconKey {
                    bucket: level.map(|l| (l.min(100) + 2) / 5 * 5),
                    tone: format::tone_for(level),
                    charging,
                    connected: true,
                }
            }
            None => IconKey {
                bucket: None,
                tone: Tone::Muted,
                charging: false,
                connected: false,
            },
        }
    }
}

type IconCache = Arc<Mutex<HashMap<(IconKey, bool), Vec<Icon>>>>;

pub struct LibreTray {
    snap: AppSnapshot,
    commands: Option<CommandSender>,
    ui_tx: UnboundedSender<BluetoothUIMessage>,
    cache: IconCache,
}

impl LibreTray {
    fn device(&self) -> Option<&DeviceSnapshot> {
        self.snap.primary()
    }

    fn cached(&self, badge: bool, render: fn(IconKey) -> Vec<Icon>) -> Vec<Icon> {
        let key = IconKey::for_snapshot(&self.snap);
        let Ok(mut cache) = self.cache.lock() else {
            return render(key);
        };
        if cache.len() > 128 {
            cache.clear();
        }
        cache
            .entry((key, badge))
            .or_insert_with(|| render(key))
            .clone()
    }

    fn send(&self, id: ControlCommandIdentifiers, value: u8) {
        if let Some(tx) = &self.commands
            && tx.send((id, vec![value])).is_err()
        {
            debug!("Control channel closed; tray command dropped");
        }
    }
}

impl ksni::Tray for LibreTray {
    fn id(&self) -> String {
        env!("CARGO_PKG_NAME").into()
    }

    fn title(&self) -> String {
        "Kase".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Hardware
    }

    fn icon_name(&self) -> String {
        ICON_NAME.into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.cached(false, |k| vec![render_full(k, 64), render_full(k, 32)])
    }

    fn overlay_icon_pixmap(&self) -> Vec<Icon> {
        if !IconKey::for_snapshot(&self.snap).connected {
            return Vec::new();
        }
        self.cached(true, |k| vec![render_badge(k, 32), render_badge(k, 16)])
    }

    fn tool_tip(&self) -> ToolTip {
        let (title, description) = format::tooltip(&self.snap, SystemTime::now());
        ToolTip {
            icon_name: String::new(),
            icon_pixmap: Vec::new(),
            title,
            description,
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.ui_tx.send(BluetoothUIMessage::OpenWindow);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let mut items: Vec<MenuItem<Self>> = Vec::new();
        let info_item = |label: String| -> MenuItem<Self> {
            StandardItem {
                label,
                enabled: false,
                ..Default::default()
            }
            .into()
        };

        match self.device() {
            Some(d) => {
                let (status, _) = format::connection_label(d);
                items.push(info_item(format!(
                    "{} — {}",
                    d.name.replace('_', "__"),
                    status
                )));
                if d.is_connected() || format::is_nearby(d) {
                    items.push(info_item(format::battery_line(d, SystemTime::now())));
                }
                let enabled = format::controls_enabled(d);
                let modes = format::listening_modes(d);
                if d.is_connected() && !modes.is_empty() {
                    items.push(MenuItem::Separator);
                    let current = d.listening_mode.and_then(ListeningMode::from_byte);
                    let selected = current
                        .and_then(|m| modes.iter().position(|x| *x == m))
                        .unwrap_or(usize::MAX);
                    let options = modes.clone();
                    items.push(
                        RadioGroup {
                            selected,
                            select: Box::new(move |this: &mut Self, i| {
                                if let Some(m) = options.get(i) {
                                    this.send(ControlCommandIdentifiers::ListeningMode, m.byte());
                                }
                            }),
                            options: modes
                                .iter()
                                .map(|m| RadioItem {
                                    label: m.label().into(),
                                    enabled,
                                    ..Default::default()
                                })
                                .collect(),
                        }
                        .into(),
                    );
                }
                if d.is_connected() && format::shows_conversation_awareness(d) {
                    let on = d.conversation_awareness.unwrap_or(false);
                    items.push(
                        CheckmarkItem {
                            label: "Conversation Awareness".into(),
                            checked: on,
                            enabled: enabled && d.conversation_awareness.is_some(),
                            activate: Box::new(move |this: &mut Self| {
                                this.send(
                                    ControlCommandIdentifiers::ConversationDetectConfig,
                                    if on { 0x02 } else { 0x01 },
                                );
                            }),
                            ..Default::default()
                        }
                        .into(),
                    );
                }
                if let Some(mic) = d.mic.filter(|_| d.is_connected()) {
                    items.push(
                        CheckmarkItem {
                            label: "Use AirPods Mic".into(),
                            checked: mic == crate::hub::MicRoute::AirPods,
                            enabled,
                            activate: Box::new(|_| crate::mic::request_toggle()),
                            ..Default::default()
                        }
                        .into(),
                    );
                }
            }
            None => {
                let (_, body) = format::tooltip(&self.snap, SystemTime::now());
                items.push(info_item(body));
            }
        }

        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Open Kase".into(),
                icon_name: "window-new".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.ui_tx.send(BluetoothUIMessage::OpenWindow);
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|_| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

/// Mirror the hub into the tray until the hub closes.
pub async fn run_tray(
    hub: Hub,
    ui_tx: UnboundedSender<BluetoothUIMessage>,
    active_commands: Arc<tokio::sync::Mutex<Option<CommandSender>>>,
) {
    let cache: IconCache = Arc::new(Mutex::new(HashMap::new()));
    let in_flatpak = crate::autostart::running_in_flatpak();
    // No StatusNotifierWatcher (e.g. GNOME without the AppIndicator extension): retry quietly,
    // the rest of the app works without a tray.
    let handle = loop {
        let tray = LibreTray {
            snap: hub.snapshot(),
            commands: active_commands.lock().await.clone(),
            ui_tx: ui_tx.clone(),
            cache: cache.clone(),
        };
        // Inside Flatpak we may not own extra bus names; the SNI works without one.
        match tray.disable_dbus_name(in_flatpak).spawn().await {
            Ok(h) => break h,
            Err(e) => {
                debug!("Tray not available yet: {e}");
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    };

    let mut rx = hub.subscribe();
    loop {
        let snap = rx.borrow_and_update().clone();
        let commands = active_commands.lock().await.clone();
        handle
            .update(move |t: &mut LibreTray| {
                t.snap = snap;
                t.commands = commands;
            })
            .await;
        if rx.changed().await.is_err() {
            return;
        }
    }
}

// ------------------------------------------------------------------ rendering

#[derive(Clone, Copy)]
struct Rgba(f32, f32, f32, f32);

const fn hex(h: u32, a: f32) -> Rgba {
    Rgba(
        ((h >> 16) & 0xFF) as f32 / 255.0,
        ((h >> 8) & 0xFF) as f32 / 255.0,
        (h & 0xFF) as f32 / 255.0,
        a,
    )
}

fn tone_color(tone: Tone, charging: bool) -> Rgba {
    match tone {
        _ if charging => hex(0x34D17A, 1.0),
        Tone::Normal => hex(0x34D17A, 1.0),
        Tone::Low => hex(0xFFB224, 1.0),
        Tone::Critical => hex(0xFF5A4E, 1.0),
        Tone::Muted => hex(0x8A8F98, 1.0),
    }
}

/// Premultiplied RGBA canvas drawn with signed distance fields (anti-aliased, no fonts).
struct Canvas {
    size: u32,
    px: Vec<[f32; 4]>,
}

impl Canvas {
    fn new(size: u32) -> Self {
        Self {
            size,
            px: vec![[0.0; 4]; (size * size) as usize],
        }
    }

    /// Paint `color` with coverage from `sdf` (negative inside), in unit (0..1) coordinates.
    fn paint(&mut self, color: Rgba, sdf: impl Fn(f32, f32) -> f32) {
        let s = self.size as f32;
        for y in 0..self.size {
            for x in 0..self.size {
                let (u, v) = ((x as f32 + 0.5) / s, (y as f32 + 0.5) / s);
                let cov = (0.5 - sdf(u, v) * s).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let a = color.3 * cov;
                let p = &mut self.px[(y * self.size + x) as usize];
                for (i, c) in [color.0, color.1, color.2].into_iter().enumerate() {
                    p[i] = c * a + p[i] * (1.0 - a);
                }
                p[3] = a + p[3] * (1.0 - a);
            }
        }
    }

    fn into_icon(self) -> Icon {
        let mut data = Vec::with_capacity(self.px.len() * 4);
        for [r, g, b, a] in self.px {
            let un = |c: f32| {
                if a > 0.0 {
                    ((c / a).clamp(0.0, 1.0) * 255.0).round() as u8
                } else {
                    0
                }
            };
            data.extend_from_slice(&[
                (a.clamp(0.0, 1.0) * 255.0).round() as u8,
                un(r),
                un(g),
                un(b),
            ]);
        }
        Icon {
            width: self.size as i32,
            height: self.size as i32,
            data,
        }
    }
}

fn circle(cx: f32, cy: f32, r: f32) -> impl Fn(f32, f32) -> f32 {
    move |x, y| ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r
}

fn capsule(ax: f32, ay: f32, bx: f32, by: f32, w: f32) -> impl Fn(f32, f32) -> f32 {
    move |x, y| {
        let (px, py, dx, dy) = (x - ax, y - ay, bx - ax, by - ay);
        let h = ((px * dx + py * dy) / (dx * dx + dy * dy).max(1e-6)).clamp(0.0, 1.0);
        ((px - dx * h).powi(2) + (py - dy * h).powi(2)).sqrt() - w / 2.0
    }
}

/// Arc of radius `r` and stroke `w` starting at 12 o'clock, clockwise over `sweep` (0..1),
/// with round caps.
fn arc(cx: f32, cy: f32, r: f32, w: f32, sweep: f32) -> impl Fn(f32, f32) -> f32 {
    let sweep = sweep.clamp(0.0, 1.0) * TAU;
    let end = (cx + r * sweep.sin(), cy - r * sweep.cos());
    move |x, y| {
        let (dx, dy) = (x - cx, y - cy);
        let ang = dx.atan2(-dy).rem_euclid(TAU);
        if ang <= sweep {
            ((dx * dx + dy * dy).sqrt() - r).abs() - w / 2.0
        } else {
            let d0 = ((x - cx).powi(2) + (y - (cy - r)).powi(2)).sqrt();
            let d1 = ((x - end.0).powi(2) + (y - end.1).powi(2)).sqrt();
            d0.min(d1) - w / 2.0
        }
    }
}

fn polygon(points: Vec<(f32, f32)>) -> impl Fn(f32, f32) -> f32 {
    move |x, y| {
        let n = points.len();
        let mut d = f32::MAX;
        let mut inside = false;
        for i in 0..n {
            let (ax, ay) = points[i];
            let (bx, by) = points[(i + 1) % n];
            d = d.min(capsule(ax, ay, bx, by, 0.0)(x, y));
            if ((ay > y) != (by > y)) && (x < (bx - ax) * (y - ay) / (by - ay) + ax) {
                inside = !inside;
            }
        }
        if inside { -d } else { d }
    }
}

fn bolt(cx: f32, cy: f32, h: f32) -> impl Fn(f32, f32) -> f32 {
    let s = h / 16.0;
    let p = |x: f32, y: f32| (cx + (x - 8.0) * s, cy + (y - 8.0) * s);
    polygon(vec![
        p(9.5, 0.0),
        p(2.5, 9.2),
        p(7.4, 9.2),
        p(6.3, 16.0),
        p(13.5, 6.6),
        p(8.6, 6.6),
    ])
}

/// Rounded box centered at (cx, cy) with half-size (hx, hy) and corner radius r.
fn rounded_box(cx: f32, cy: f32, hx: f32, hy: f32, r: f32) -> impl Fn(f32, f32) -> f32 {
    move |x, y| {
        let qx = (x - cx).abs() - (hx - r);
        let qy = (y - cy).abs() - (hy - r);
        (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r
    }
}

/// The app's mark, an AirPods case: same 32-unit geometry as the overlay glyph and
/// `assets/icons/`. The lid seam and status light are cut out. `grow` fattens it (halo).
fn case_mark(c: &mut Canvas, color: Rgba, grow: f32) {
    let k = 0.62 / 32.0;
    let p = |x: f32, y: f32| (0.5 + (x - 16.0) * k, 0.5 + (y - 16.0) * k);
    let (cx, cy) = p(16.0, 16.0);
    let shell = rounded_box(cx, cy, 12.0 * k + grow, 10.0 * k + grow, 7.0 * k + grow);
    let (_, seam_y) = p(0.0, 13.2);
    let (lx, ly) = p(16.0, 19.6);
    let cut = grow <= 0.0;
    c.paint(color, move |x, y| {
        let d = shell(x, y);
        if !cut {
            return d;
        }
        let seam = (y - seam_y).abs() - 0.9 * k;
        let led = ((x - lx).powi(2) + (y - ly).powi(2)).sqrt() - 1.7 * k;
        d.max(-seam.min(led))
    });
}

/// Full fallback icon: battery ring around the case mark.
pub fn render_full(key: IconKey, size: u32) -> Icon {
    let mut c = Canvas::new(size);
    let glyph = if key.connected {
        hex(0xECEEF1, 1.0)
    } else {
        hex(0x8A8F98, 1.0)
    };
    let halo = hex(0x000000, 0.35);
    let (r, w) = (0.40, 0.085);
    c.paint(halo, arc(0.5, 0.5, r, w + 0.05, 1.0));
    c.paint(
        hex(0x8A8F98, if key.connected { 0.45 } else { 0.6 }),
        arc(0.5, 0.5, r, w, 1.0),
    );
    if let Some(b) = key.bucket.filter(|_| key.connected) {
        c.paint(
            tone_color(key.tone, key.charging),
            arc(0.5, 0.5, r, w, b as f32 / 100.0),
        );
    }
    case_mark(&mut c, halo, 0.025);
    case_mark(&mut c, glyph, 0.0);
    if key.charging {
        c.paint(halo, circle(0.78, 0.78, 0.2));
        c.paint(tone_color(Tone::Normal, true), bolt(0.78, 0.78, 0.3));
    }
    c.into_icon()
}

/// Small overlay badge: dark disc with the battery arc, bolt inside while charging.
pub fn render_badge(key: IconKey, size: u32) -> Icon {
    let mut c = Canvas::new(size);
    c.paint(hex(0x16181C, 0.92), circle(0.5, 0.5, 0.5));
    let (r, w) = (0.33, 0.16);
    c.paint(hex(0xFFFFFF, 0.18), arc(0.5, 0.5, r, w, 1.0));
    let color = tone_color(key.tone, key.charging);
    if let Some(b) = key.bucket {
        c.paint(color, arc(0.5, 0.5, r, w, b as f32 / 100.0));
    }
    if key.charging {
        c.paint(color, bolt(0.5, 0.5, 0.42));
    }
    c.into_icon()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::battery::ComponentBattery;
    use crate::hub::LinkState;

    fn snap(level: u8, charging: bool, link: LinkState) -> AppSnapshot {
        let mut d = DeviceSnapshot::new("AA".into(), "Pods".into());
        d.link = link;
        let c = ComponentBattery {
            level: Some(level),
            charging,
            present: true,
            stale: false,
            last_seen: None,
        };
        d.battery.left = c.clone();
        d.battery.right = c;
        let mut s = AppSnapshot::default();
        s.devices.insert("AA".into(), d);
        s
    }

    #[test]
    fn icon_key_buckets() {
        let k = IconKey::for_snapshot(&snap(83, false, LinkState::Connected));
        assert_eq!(k.bucket, Some(85));
        assert_eq!(k.tone, Tone::Normal);
        assert!(k.connected);
        assert_eq!(
            IconKey::for_snapshot(&snap(81, false, LinkState::Connected)),
            IconKey::for_snapshot(&snap(79, false, LinkState::Connected))
        );
        let low = IconKey::for_snapshot(&snap(9, true, LinkState::Connected));
        assert_eq!(low.tone, Tone::Critical);
        assert!(low.charging);
        let off = IconKey::for_snapshot(&snap(50, false, LinkState::Disconnected));
        assert!(!off.connected);
        assert_eq!(off.bucket, None);
    }

    /// `LIBREPODS_ICON_DUMP=/some/dir cargo test dump_tray_icons -- --ignored` writes previews.
    #[test]
    #[ignore]
    fn dump_tray_icons() {
        let Some(dir) = std::env::var_os("LIBREPODS_ICON_DUMP") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let cases = [
            ("full-80", snap(80, false, LinkState::Connected)),
            ("full-15-charging", snap(15, true, LinkState::Connected)),
            ("full-8", snap(8, false, LinkState::Connected)),
            ("full-off", snap(50, false, LinkState::Disconnected)),
        ];
        for (name, s) in cases {
            let k = IconKey::for_snapshot(&s);
            for (kind, icon) in [("icon", render_full(k, 64)), ("badge", render_badge(k, 32))] {
                let rgba: Vec<u8> = icon
                    .data
                    .chunks(4)
                    .flat_map(|p| [p[1], p[2], p[3], p[0]])
                    .collect();
                let img = image::RgbaImage::from_raw(icon.width as u32, icon.height as u32, rgba)
                    .unwrap();
                img.save(dir.join(format!("{name}-{kind}.png"))).unwrap();
            }
        }
    }

    #[test]
    fn renders_have_expected_size_and_content() {
        let key = IconKey::for_snapshot(&snap(50, true, LinkState::Connected));
        for size in [16, 32, 64] {
            let icon = render_full(key, size);
            assert_eq!(icon.data.len(), (size * size * 4) as usize);
            assert!(
                icon.data.chunks(4).any(|p| p[0] > 200),
                "something opaque drawn"
            );
            let badge = render_badge(key, size);
            assert_eq!(badge.width, size as i32);
        }
    }
}

#[cfg(test)]
#[path = "tray_tests.rs"]
mod menu_tests;
