//! `librepods-overlay`: a click-through battery island, in the manner of the iPhone's
//! Dynamic Island, drawn on its own wlr-layer-shell surface (Layer::Overlay, above
//! fullscreen windows on KWin).
//!
//! Anti-cheat safety: this is an ordinary Wayland client. It never touches,
//! inspects or injects into other processes; it only draws its own surface.
//!
//! It is driven by the main app over stdin (JSON lines, see
//! `docs/ARCHITECTURE.md`). Exit codes: 0 normal (quit / stdin EOF),
//! 1 runtime error, 3 no layer-shell / no Wayland display.
//!
//! While hidden the layer surface is destroyed (not just made transparent), so
//! nothing sits over a fullscreen game and direct scanout keeps working. While shown
//! it redraws only during a transition, paced by the compositor's frame callbacks, and
//! otherwise sleeps until the next thing it has to do (no polling).

mod demo;
mod glyphs;
mod island;
mod logic;
mod probe;
mod purist;
mod spring;
mod view;

use std::io::BufRead;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use iced::widget::canvas;
use iced::window::Id;
use iced::{Color, Element, Length, Subscription, Task};
use iced_layershell::build_pattern::daemon;
use iced_layershell::reexport::{
    Anchor, KeyboardInteractivity, Layer, NewLayerShellSettings, OutputOption,
};
use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};
use iced_layershell::to_layer_message;
use log::{debug, info, warn};

use kase::overlay_protocol::{
    EXIT_UNSUPPORTED, OverlayCommand, OverlayConfig, OverlayMode, OverlayPosition, OverlaySize,
    OverlayState, OverlayStyle,
};
use logic::{Controller, SurfaceAction};

const NAMESPACE: &str = "librepods-overlay";

#[derive(Debug, Clone, Default)]
struct Args {
    demo: bool,
    demo_auto: bool,
    demo_compact: bool,
    demo_step: Option<usize>,
    story: bool,
    mode: Option<OverlayMode>,
    position: Option<OverlayPosition>,
    style: Option<OverlayStyle>,
    size: Option<OverlaySize>,
}

fn usage() -> &'static str {
    "librepods-overlay: AirPods battery overlay (driven by librepods over stdin)\n\n\
     Options:\n  \
       --demo              cycle through sample states (always visible)\n  \
       --demo-auto         like --demo, but in auto mode (pops up, then unmaps)\n  \
       --demo-compact      like --demo, but in compact mode (expands on changes)\n  \
       --step N            with --demo: show only sample N (0-7)\n  \
       --demo-story        play a scripted tour of every island state (for recordings)\n  \
       --mode M            with --demo-story: auto|compact|always (default compact)\n  \
       --position P        top-left|top-center|top-right|bottom-left|bottom-center|bottom-right\n  \
       --style S           with a demo: classic|purist\n  \
       --size S            with a demo: small|medium|large|extra_large\n  \
       --probe             check for layer-shell support and exit (0 or 3)\n"
}

#[to_layer_message(multi)]
#[derive(Debug, Clone)]
enum Message {
    Cmd(OverlayCommand),
    StdinClosed,
    Tick(Instant),
    /// A scheduled deadline arrived.
    Wake(Instant),
    DemoStep,
    WindowClosed(Id),
}

struct Overlay {
    ctl: Controller,
    window: Option<Id>,
    /// Surface size of the current window.
    size: (u32, u32),
    demo: Option<DemoState>,
    last_lost: Option<Instant>,
    /// The deadline a wake-up is already scheduled for.
    wake_at: Option<Instant>,
    /// Playing `--demo-story`: stdin is not the protocol.
    story: bool,
}

struct DemoState {
    step: usize,
    fixed: bool,
    states: Vec<(&'static str, OverlayState)>,
}

fn anchor_for(p: OverlayPosition) -> Anchor {
    match p {
        OverlayPosition::TopLeft => Anchor::Top | Anchor::Left,
        OverlayPosition::TopCenter => Anchor::Top,
        OverlayPosition::TopRight => Anchor::Top | Anchor::Right,
        OverlayPosition::BottomLeft => Anchor::Bottom | Anchor::Left,
        OverlayPosition::BottomCenter => Anchor::Bottom,
        OverlayPosition::BottomRight => Anchor::Bottom | Anchor::Right,
    }
}

/// Layer-shell margin (top, right, bottom, left). The surface carries `PAD`
/// transparent pixels around the pill, so subtract them from the visual margin.
fn margin_for(cfg: &OverlayConfig) -> (i32, i32, i32, i32) {
    let m = (cfg.margin - view::PAD as i32).max(0);
    (m, m, m, m)
}

impl Overlay {
    fn new(args: &Args) -> (Self, Task<Message>) {
        let mut cfg = OverlayConfig::default();
        if let Some(p) = args.position {
            cfg.position = p;
        }
        if let Some(s) = args.style {
            cfg.style = s;
        }
        if let Some(s) = args.size {
            cfg.size = s;
        }
        let demo = args.demo.then(|| DemoState {
            step: args.demo_step.unwrap_or(0),
            fixed: args.demo_step.is_some(),
            states: demo::states(),
        });
        if demo.is_some() {
            cfg.mode = if args.demo_auto {
                OverlayMode::Auto
            } else if args.demo_compact {
                OverlayMode::Compact
            } else {
                OverlayMode::Always
            };
        }
        let mut me = Self {
            ctl: Controller::new(cfg),
            window: None,
            size: (0, 0),
            demo,
            last_lost: None,
            wake_at: None,
            story: args.story,
        };
        let task = if me.demo.is_some() {
            me.demo_apply()
        } else if args.story {
            me.ctl.config.mode = args.mode.unwrap_or(OverlayMode::Compact);
            Task::batch(demo::story().into_iter().map(|(secs, cmd)| {
                Task::perform(
                    tokio::time::sleep(Duration::from_secs_f32(secs)),
                    move |_| Message::Cmd(cmd.clone()),
                )
            }))
        } else {
            Task::none()
        };
        (me, task)
    }

    fn demo_apply(&mut self) -> Task<Message> {
        let Some(d) = &self.demo else {
            return Task::none();
        };
        let len = d.states.len().max(1);
        let Some((label, state)) = d.states.get(d.step % len).cloned() else {
            return Task::none();
        };
        info!("demo: {label}");
        let now = Instant::now();
        self.ctl.apply(OverlayCommand::State { state }, now);
        self.ctl.apply(
            OverlayCommand::Show {
                reason: "demo".into(),
            },
            now,
        );
        self.sync(now)
    }

    fn layer_settings(&self) -> NewLayerShellSettings {
        let cfg = &self.ctl.config;
        NewLayerShellSettings {
            size: Some(self.size),
            // Overlay is above fullscreen windows in KWin; Top is not.
            layer: Layer::Overlay,
            anchor: anchor_for(cfg.position),
            exclusive_zone: Some(0),
            margin: Some(margin_for(cfg)),
            keyboard_interactivity: KeyboardInteractivity::None,
            output_option: OutputOption::Active,
            // Empty input region: clicks go straight through to the game/desktop.
            events_transparent: true,
            namespace: Some(NAMESPACE.into()),
        }
    }

    /// Advances the controller, maps/unmaps the layer surface, and schedules the next
    /// wake-up.
    fn sync(&mut self, now: Instant) -> Task<Message> {
        let mut tasks = vec![];
        match self.ctl.tick(now) {
            SurfaceAction::Map => {
                self.size = view::surface_size(self.ctl.extent());
                let id = Id::unique();
                self.window = Some(id);
                debug!("mapping layer surface {id:?}");
                tasks.push(Task::done(Message::NewLayerShell {
                    settings: self.layer_settings(),
                    id,
                }));
            }
            SurfaceAction::Unmap => {
                // Destroys the zwlr_layer_surface_v1 and wl_surface; with no
                // windows left iced_layershell also drops its renderer.
                if let Some(id) = self.window.take() {
                    debug!("unmapping layer surface {id:?}");
                    tasks.push(iced::window::close(id));
                }
            }
            SurfaceAction::None => {}
        }
        tasks.push(self.schedule_wake(now));
        Task::batch(tasks)
    }

    /// One timer for the next deadline (a pop-up ending), instead of polling.
    fn schedule_wake(&mut self, now: Instant) -> Task<Message> {
        let next = self.ctl.next_deadline().filter(|t| *t > now);
        if next == self.wake_at {
            return Task::none();
        }
        self.wake_at = next;
        match next {
            Some(at) => Task::perform(
                tokio::time::sleep_until(tokio::time::Instant::from_std(at)),
                move |_| Message::Wake(at),
            ),
            None => Task::none(),
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        let now = Instant::now();
        match message {
            Message::Cmd(OverlayCommand::Quit) => iced::exit(),
            Message::StdinClosed => {
                if self.demo.is_some() || self.story {
                    Task::none()
                } else {
                    info!("stdin closed, exiting");
                    iced::exit()
                }
            }
            Message::Cmd(cmd) => {
                let old_cfg = self.ctl.config.clone();
                self.ctl.apply(cmd, now);
                let mut tasks = vec![];
                if let Some(id) = self.window
                    && self.ctl.config.size != old_cfg.size
                {
                    // The surface is sized for the island and never resized while shown
                    // (see `Controller::extent`), so a new size gets a new surface.
                    tasks.push(iced::window::close(id));
                    self.window = None;
                    self.ctl.surface_lost();
                }
                if let Some(id) = self.window {
                    let cfg = &self.ctl.config;
                    if cfg.position != old_cfg.position {
                        tasks.push(Task::done(Message::AnchorChange {
                            id,
                            anchor: anchor_for(cfg.position),
                        }));
                    }
                    if cfg.position != old_cfg.position || cfg.margin != old_cfg.margin {
                        tasks.push(Task::done(Message::MarginChange {
                            id,
                            margin: margin_for(cfg),
                        }));
                    }
                }
                tasks.push(self.sync(now));
                Task::batch(tasks)
            }
            Message::Tick(at) => self.sync(at.max(now)),
            Message::Wake(at) => {
                if self.wake_at == Some(at) {
                    self.wake_at = None;
                }
                self.sync(now)
            }
            Message::DemoStep => {
                if let Some(d) = &mut self.demo {
                    d.step = d.step.wrapping_add(1);
                }
                self.demo_apply()
            }
            Message::WindowClosed(id) => {
                if self.window == Some(id) {
                    warn!("compositor closed the overlay surface");
                    self.window = None;
                    self.ctl.surface_lost();
                    // Don't fight a compositor that keeps closing us.
                    if self
                        .last_lost
                        .is_some_and(|t| now.duration_since(t) < Duration::from_secs(5))
                    {
                        self.ctl.apply(OverlayCommand::Hide, now);
                    }
                    self.last_lost = Some(now);
                }
                self.sync(now)
            }
            // Layer-shell actions are consumed by iced_layershell before reaching us.
            _ => Task::none(),
        }
    }

    fn view(&self, _id: Id) -> Element<'_, Message> {
        let now = Instant::now();
        if self.ctl.display_state(now).is_none() {
            return iced::widget::Space::new().into();
        }
        let island = view::Island {
            ctl: &self.ctl,
            now,
        };
        let surface = iced::Size::new(self.size.0 as f32, self.size.1 as f32);
        let body = island.purist_body(surface);
        let content = canvas(island).width(Length::Fill).height(Length::Fill);
        match body {
            // The purist body is a shader underneath; the canvas draws the content on top.
            Some(body) => iced::widget::stack![
                iced::widget::shader(body)
                    .width(Length::Fill)
                    .height(Length::Fill),
                content,
            ]
            .into(),
            None => content.into(),
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let now = Instant::now();
        let mut subs = vec![iced::window::close_events().map(Message::WindowClosed)];
        if self.demo.is_none() && !self.story {
            subs.push(Subscription::run(stdin_stream));
        }
        if self.ctl.is_animating(now) {
            // Paced by the compositor: one step per displayed frame, at the monitor's
            // refresh rate, and none while nothing moves. The slow timer only keeps a
            // transition going if frame callbacks stall (e.g. the output turned off).
            if self.window.is_some() {
                subs.push(iced::window::frames().map(Message::Tick));
            }
            subs.push(iced::time::every(Duration::from_millis(100)).map(Message::Tick));
        }
        if let Some(d) = &self.demo
            && !d.fixed
        {
            let every = if self.ctl.config.mode != OverlayMode::Always {
                6000
            } else {
                2500
            };
            subs.push(iced::time::every(Duration::from_millis(every)).map(|_| Message::DemoStep));
        }
        Subscription::batch(subs)
    }
}

/// Reads protocol lines from stdin on a plain thread and forwards them to iced.
fn stdin_stream() -> impl futures::Stream<Item = Message> {
    iced::stream::channel(
        64,
        async |mut output: futures::channel::mpsc::Sender<Message>| {
            let (tx, mut rx) = futures::channel::mpsc::unbounded::<Message>();
            let spawned = std::thread::Builder::new()
                .name("overlay-stdin".into())
                .spawn(move || {
                    let stdin = std::io::stdin();
                    for line in stdin.lock().lines() {
                        let Ok(line) = line else { break };
                        match OverlayCommand::parse_line(&line) {
                            Ok(Some(cmd)) => {
                                if tx.unbounded_send(Message::Cmd(cmd)).is_err() {
                                    return;
                                }
                            }
                            Ok(None) => {}
                            Err(e) => warn!("ignoring bad protocol line: {e}"),
                        }
                    }
                    let _ = tx.unbounded_send(Message::StdinClosed);
                });
            if let Err(e) = spawned {
                warn!("cannot start stdin reader: {e}");
                let _ = output.send(Message::StdinClosed).await;
                return;
            }
            while let Some(msg) = rx.next().await {
                if output.send(msg).await.is_err() {
                    break;
                }
            }
        },
    )
}

fn parse_args() -> Result<(Args, bool), String> {
    let mut args = Args::default();
    let mut probe_only = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--demo" => args.demo = true,
            "--demo-auto" => {
                args.demo = true;
                args.demo_auto = true;
            }
            "--demo-compact" => {
                args.demo = true;
                args.demo_compact = true;
            }
            "--demo-story" => args.story = true,
            "--mode" => {
                let m = it.next().ok_or("--mode needs a value")?;
                let m: OverlayMode = serde_json::from_value(serde_json::Value::String(m))
                    .map_err(|_| "bad --mode value".to_string())?;
                args.mode = Some(m);
            }
            "--step" => {
                let n = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or("--step needs a number")?;
                args.demo_step = Some(n);
            }
            "--position" => {
                let p = it.next().ok_or("--position needs a value")?;
                let p: OverlayPosition = serde_json::from_value(serde_json::Value::String(p))
                    .map_err(|_| "bad --position value".to_string())?;
                args.position = Some(p);
            }
            "--style" => {
                let v = it.next().ok_or("--style needs a value")?;
                args.style = Some(
                    serde_json::from_value(serde_json::Value::String(v))
                        .map_err(|_| "bad --style value".to_string())?,
                );
            }
            "--size" => {
                let v = it.next().ok_or("--size needs a value")?;
                args.size = Some(
                    serde_json::from_value(serde_json::Value::String(v))
                        .map_err(|_| "bad --size value".to_string())?,
                );
            }
            "--probe" => probe_only = true,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok((args, probe_only))
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Render with Vulkan only (skips a noisy EGL probe). The GPU is left to wgpu's default:
    // forcing the integrated one breaks on hybrid PCs where the compositor runs on the
    // discrete card, because it can't import buffers drawn on the other GPU. Both can be
    // overridden from outside (WGPU_BACKEND, WGPU_POWER_PREF).
    if std::env::var_os("WGPU_BACKEND").is_none() {
        // SAFETY: first thing in main, before any other thread exists.
        unsafe { std::env::set_var("WGPU_BACKEND", "vulkan") };
    }

    let (args, probe_only) = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("{e}\n");
            }
            eprint!("{}", usage());
            return ExitCode::from(if e.is_empty() { 0 } else { 2 });
        }
    };

    // layershellev panics if zwlr_layer_shell_v1 is missing, so check first.
    match probe::probe() {
        probe::Probe::Supported => {}
        probe::Probe::NoDisplay(why) => {
            warn!("no Wayland display ({why}); overlay unsupported");
            return ExitCode::from(EXIT_UNSUPPORTED as u8);
        }
        probe::Probe::Missing(globals) => {
            warn!("compositor lacks {globals:?}; overlay unsupported");
            return ExitCode::from(EXIT_UNSUPPORTED as u8);
        }
    }
    if probe_only {
        info!("layer-shell available");
        return ExitCode::SUCCESS;
    }

    // No text input: skip the clipboard worker thread.
    iced_layershell::disable_clipboard();

    let result = daemon(
        move || Overlay::new(&args),
        || NAMESPACE.to_string(),
        Overlay::update,
        Overlay::view,
    )
    .subscription(Overlay::subscription)
    .style(|_, _| iced::theme::Style {
        background_color: Color::TRANSPARENT,
        text_color: Color::WHITE,
    })
    .settings(Settings {
        id: Some(NAMESPACE.into()),
        // Inter for the purist island (the classic one keeps the system sans).
        fonts: vec![
            include_bytes!("../../../assets/font/Inter-Medium.ttf")
                .as_slice()
                .into(),
            include_bytes!("../../../assets/font/Inter-SemiBold.ttf")
                .as_slice()
                .into(),
        ],
        antialiasing: true,
        layer_settings: LayerShellSettings {
            // Start with no surface at all; surfaces are created on demand.
            start_mode: StartMode::Background,
            layer: Layer::Overlay,
            keyboard_interactivity: KeyboardInteractivity::None,
            events_transparent: true,
            exclusive_zone: 0,
            ..Default::default()
        },
        ..Default::default()
    })
    .run();

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log::error!("overlay failed: {e}");
            ExitCode::from(1)
        }
    }
}
