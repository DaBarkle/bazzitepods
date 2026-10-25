//! `librepods-overlay`: a tiny, click-through battery pill drawn on its own
//! wlr-layer-shell surface (Layer::Overlay, above fullscreen windows on KWin).
//!
//! Anti-cheat safety: this is an ordinary Wayland client. It never touches,
//! inspects or injects into other processes; it only draws its own surface.
//!
//! It is driven by the main app over stdin (JSON lines, see
//! `docs/ARCHITECTURE.md`). Exit codes: 0 normal (quit / stdin EOF),
//! 1 runtime error, 3 no layer-shell / no Wayland display.
//!
//! While hidden the layer surface is destroyed (not just made transparent), so
//! nothing sits over a fullscreen game and direct scanout keeps working.

#[path = "../overlay_protocol.rs"]
mod overlay_protocol;

#[path = "librepods-overlay/demo.rs"]
mod demo;
#[path = "librepods-overlay/logic.rs"]
mod logic;
#[path = "librepods-overlay/probe.rs"]
mod probe;
#[path = "librepods-overlay/view.rs"]
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

use logic::{Controller, SurfaceAction};
use overlay_protocol::{
    EXIT_UNSUPPORTED, OverlayCommand, OverlayConfig, OverlayMode, OverlayPosition, OverlayState,
};

const NAMESPACE: &str = "librepods-overlay";

#[derive(Debug, Clone, Default)]
struct Args {
    demo: bool,
    demo_auto: bool,
    demo_step: Option<usize>,
    position: Option<OverlayPosition>,
}

fn usage() -> &'static str {
    "librepods-overlay: AirPods battery overlay (driven by librepods over stdin)\n\n\
     Options:\n  \
       --demo              cycle through sample states (always visible)\n  \
       --demo-auto         like --demo, but in auto mode (pops up, then unmaps)\n  \
       --step N            with --demo: show only sample N (0-6)\n  \
       --position P        top-left|top-center|top-right|bottom-left|bottom-center|bottom-right\n  \
       --probe             check for layer-shell support and exit (0 or 3)\n"
}

#[to_layer_message(multi)]
#[derive(Debug, Clone)]
enum Message {
    Cmd(OverlayCommand),
    StdinClosed,
    Tick(Instant),
    DemoStep,
    WindowClosed(Id),
}

struct Overlay {
    ctl: Controller,
    window: Option<Id>,
    /// Surface size of the current window, to resize when the layout changes.
    size: (u32, u32),
    demo: Option<DemoState>,
    last_lost: Option<Instant>,
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
        let demo = args.demo.then(|| DemoState {
            step: args.demo_step.unwrap_or(0),
            fixed: args.demo_step.is_some(),
            states: demo::states(),
        });
        if demo.is_some() {
            cfg.mode = if args.demo_auto {
                OverlayMode::Auto
            } else {
                OverlayMode::Always
            };
        }
        let mut me = Self {
            ctl: Controller::new(cfg),
            window: None,
            size: view::surface_size(None),
            demo,
            last_lost: None,
        };
        let task = if me.demo.is_some() {
            me.demo_apply()
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

    /// Advances the controller and maps/unmaps the layer surface as needed.
    fn sync(&mut self, now: Instant) -> Task<Message> {
        match self.ctl.tick(now) {
            SurfaceAction::Map => {
                self.size = view::surface_size(self.ctl.display_state(now).as_ref());
                let id = Id::unique();
                self.window = Some(id);
                debug!("mapping layer surface {id:?}");
                Task::done(Message::NewLayerShell {
                    settings: self.layer_settings(),
                    id,
                })
            }
            SurfaceAction::Unmap => match self.window.take() {
                // Destroys the zwlr_layer_surface_v1 and wl_surface; with no
                // windows left iced_layershell also drops its renderer.
                Some(id) => {
                    debug!("unmapping layer surface {id:?}");
                    iced::window::close(id)
                }
                None => Task::none(),
            },
            SurfaceAction::None => Task::none(),
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        let now = Instant::now();
        match message {
            Message::Cmd(OverlayCommand::Quit) => iced::exit(),
            Message::StdinClosed => {
                if self.demo.is_some() {
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
                    let size = view::surface_size(self.ctl.display_state(now).as_ref());
                    if size != self.size {
                        self.size = size;
                        tasks.push(Task::done(Message::SizeChange { id, size }));
                    }
                }
                tasks.push(self.sync(now));
                Task::batch(tasks)
            }
            Message::Tick(at) => self.sync(at.max(now)),
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
        let Some(state) = self.ctl.display_state(Instant::now()) else {
            return iced::widget::Space::new().into();
        };
        canvas(view::Pill {
            state,
            visual: self.ctl.visual(),
            expand: self.ctl.expansion(),
            opacity: self.ctl.effective_opacity(),
            glass: self.ctl.config.glass,
            position: self.ctl.config.position,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let now = Instant::now();
        let mut subs = vec![iced::window::close_events().map(Message::WindowClosed)];
        if self.demo.is_none() {
            subs.push(Subscription::run(stdin_stream));
        }
        if self.ctl.is_animating(now) {
            subs.push(iced::time::every(Duration::from_millis(16)).map(Message::Tick));
        } else if self.ctl.has_deadline() {
            subs.push(iced::time::every(Duration::from_millis(200)).map(Message::Tick));
        }
        if let Some(d) = &self.demo
            && !d.fixed
        {
            let every = if self.ctl.config.mode == OverlayMode::Auto {
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
            "--probe" => probe_only = true,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok((args, probe_only))
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

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
