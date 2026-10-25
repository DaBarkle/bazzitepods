//! Small libpulse wrapper (talks to pipewire-pulse on Bazzite).
//!
//! Every operation opens a short-lived connection, runs the mainloop with a real
//! blocking poll bounded by an overall deadline (no busy-spinning on
//! `iterate(false)`), and is executed on tokio's blocking pool by the async
//! [`PulseApi`] implementation. The [`PulseApi`] trait is what the audio state
//! machine uses, so tests can swap in a fake backend.

use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::{Context, FlagSet as ContextFlagSet, State as ContextState};
use libpulse_binding::def::SinkState as PaSinkState;
use libpulse_binding::mainloop::standard::Mainloop;
use libpulse_binding::operation::{Operation, State as OperationState};
use libpulse_binding::proplist::Proplist;
use libpulse_binding::time::MicroSeconds;
use libpulse_binding::volume::{ChannelVolumes, Volume};
use std::cell::{Cell, RefCell};
use std::fmt;
use std::future::Future;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Overall deadline for one libpulse operation, including connecting.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileInfo {
    pub name: String,
    pub available: bool,
    pub priority: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardInfo {
    pub index: u32,
    pub name: String,
    /// Bluetooth address (upper case, colon separated) when this is a bluez card.
    pub mac: Option<String>,
    pub profiles: Vec<ProfileInfo>,
    pub active_profile: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkState {
    Running,
    Idle,
    Suspended,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkInfo {
    pub index: u32,
    pub name: String,
    pub card: Option<u32>,
    pub mac: Option<String>,
    pub state: SinkState,
    /// `api.bluez5.codec` as published by PipeWire, if any.
    pub codec: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkInputInfo {
    pub index: u32,
    pub sink: u32,
    pub corked: bool,
    /// `application.name` of the stream's client, if published.
    pub app: Option<String>,
    /// `media.role`, e.g. "music", "event" (notification sounds), "phone".
    pub role: Option<String>,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInfo {
    pub index: u32,
    pub card: Option<u32>,
    /// True for monitor sources (they never mean "the mic is in use").
    pub is_monitor: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceOutputInfo {
    pub index: u32,
    pub source: u32,
    pub corked: bool,
}

/// Everything the audio guard needs to know, read in one round trip.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PulseSnapshot {
    pub cards: Vec<CardInfo>,
    pub sinks: Vec<SinkInfo>,
    pub sink_inputs: Vec<SinkInputInfo>,
    pub sources: Vec<SourceInfo>,
    pub source_outputs: Vec<SourceOutputInfo>,
    pub default_sink: Option<String>,
}

impl PulseSnapshot {
    /// The bluez card for `mac` (case-insensitive).
    pub fn card_for(&self, mac: &str) -> Option<&CardInfo> {
        self.cards.iter().find(|c| {
            c.mac
                .as_deref()
                .is_some_and(|m| m.eq_ignore_ascii_case(mac))
        })
    }

    /// The sink belonging to `card` (by card index, falling back to the address).
    pub fn sink_for_card(&self, card: &CardInfo) -> Option<&SinkInfo> {
        self.sinks
            .iter()
            .find(|s| s.card == Some(card.index))
            .or_else(|| {
                let mac = card.mac.as_deref()?;
                self.sinks.iter().find(|s| {
                    s.mac
                        .as_deref()
                        .is_some_and(|m| m.eq_ignore_ascii_case(mac))
                })
            })
    }

    pub fn sink_by_name(&self, name: &str) -> Option<&SinkInfo> {
        self.sinks.iter().find(|s| s.name == name)
    }

    /// Number of playing (uncorked) streams on a sink.
    pub fn uncorked_inputs_on(&self, sink_index: u32) -> usize {
        self.sink_inputs
            .iter()
            .filter(|i| i.sink == sink_index && !i.corked)
            .count()
    }

    /// Streams that are playing right now, leaving out system sounds and background
    /// streams that never stop (EasyEffects re-plays every app through its own stream;
    /// Discord keeps WebRTC voice streams open), so only real players count.
    pub fn playing_streams(&self) -> Vec<u32> {
        self.sink_inputs
            .iter()
            .filter(|i| !i.corked && !i.app.as_deref().is_some_and(is_background_app))
            // Notification and other system sounds never mean "the user pressed play".
            .filter(|i| {
                !i.role
                    .as_deref()
                    .is_some_and(|r| r == "event" || r == "notification")
            })
            .map(|i| i.index)
            .collect()
    }

    /// Whether something is recording from a (non-monitor) source of this card.
    pub fn is_recording_from_card(&self, card_index: u32) -> bool {
        self.source_outputs.iter().any(|o| {
            !o.corked
                && self
                    .sources
                    .iter()
                    .any(|s| s.index == o.source && s.card == Some(card_index) && !s.is_monitor)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PulseError {
    Connect(String),
    Timeout(&'static str),
    Failed(String),
}

impl fmt::Display for PulseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PulseError::Connect(e) => write!(f, "cannot connect to the sound server: {e}"),
            PulseError::Timeout(what) => write!(f, "sound server timed out ({what})"),
            PulseError::Failed(e) => write!(f, "sound server operation failed: {e}"),
        }
    }
}

impl std::error::Error for PulseError {}

/// The libpulse operations the audio guard needs. Implemented by [`LibPulse`]
/// and by a fake in the unit tests.
pub trait PulseApi: Send + Sync + 'static {
    fn snapshot(&self) -> impl Future<Output = Result<PulseSnapshot, PulseError>> + Send;
    fn set_card_profile(
        &self,
        card_index: u32,
        profile: &str,
    ) -> impl Future<Output = Result<(), PulseError>> + Send;
    fn set_default_sink(&self, name: &str) -> impl Future<Output = Result<(), PulseError>> + Send;
    fn move_sink_input(
        &self,
        input_index: u32,
        sink_index: u32,
    ) -> impl Future<Output = Result<(), PulseError>> + Send;
    fn set_sink_input_mute(
        &self,
        input_index: u32,
        mute: bool,
    ) -> impl Future<Output = Result<(), PulseError>> + Send;
}

/// Real backend.
#[derive(Debug, Clone, Default)]
pub struct LibPulse;

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, PulseError> + Send + 'static,
) -> Result<T, PulseError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| PulseError::Failed(format!("blocking task failed: {e}")))?
}

impl PulseApi for LibPulse {
    async fn snapshot(&self) -> Result<PulseSnapshot, PulseError> {
        blocking(|| snapshot_sync(DEFAULT_DEADLINE)).await
    }

    async fn set_card_profile(&self, card_index: u32, profile: &str) -> Result<(), PulseError> {
        let profile = profile.to_string();
        blocking(move || set_card_profile_sync(card_index, &profile, DEFAULT_DEADLINE)).await
    }

    async fn set_default_sink(&self, name: &str) -> Result<(), PulseError> {
        let name = name.to_string();
        blocking(move || set_default_sink_sync(&name, DEFAULT_DEADLINE)).await
    }

    async fn move_sink_input(&self, input_index: u32, sink_index: u32) -> Result<(), PulseError> {
        blocking(move || move_sink_input_sync(input_index, sink_index, DEFAULT_DEADLINE)).await
    }

    async fn set_sink_input_mute(&self, input_index: u32, mute: bool) -> Result<(), PulseError> {
        blocking(move || set_sink_input_mute_sync(input_index, mute, DEFAULT_DEADLINE)).await
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Parses `AA:BB:CC:DD:EE:FF` / `aa_bb_cc_dd_ee_ff` (optionally embedded at the start
/// of a longer string) into upper-case colon form.
fn is_background_app(app: &str) -> bool {
    let app = app.to_ascii_lowercase();
    ["easyeffects", "pulseeffects", "webrtc", "discord"]
        .iter()
        .any(|a| app.contains(a))
}

pub fn normalize_mac(s: &str) -> Option<String> {
    let candidate: String = s.chars().take(17).collect();
    if candidate.len() != 17 {
        return None;
    }
    let mut out = String::with_capacity(17);
    for (i, ch) in candidate.chars().enumerate() {
        if i % 3 == 2 {
            if ch != ':' && ch != '_' && ch != '-' {
                return None;
            }
            out.push(':');
        } else if ch.is_ascii_hexdigit() {
            out.push(ch.to_ascii_uppercase());
        } else {
            return None;
        }
    }
    Some(out)
}

/// Finds the Bluetooth address of a card/sink/source from its properties or name.
pub fn bluez_mac_from(name: &str, prop: impl Fn(&str) -> Option<String>) -> Option<String> {
    if let Some(m) = prop("api.bluez5.address")
        .as_deref()
        .and_then(normalize_mac)
    {
        return Some(m);
    }
    if let Some(m) = prop("device.string").as_deref().and_then(normalize_mac) {
        return Some(m);
    }
    if let Some(path) = prop("api.bluez5.path").or_else(|| prop("bluez.path"))
        && let Some(pos) = path.find("dev_")
        && let Some(m) = normalize_mac(&path[pos + 4..])
    {
        return Some(m);
    }
    for prefix in [
        "bluez_card.",
        "bluez_output.",
        "bluez_input.",
        "bluez_sink.",
        "bluez_source.",
    ] {
        if let Some(rest) = name.strip_prefix(prefix) {
            return normalize_mac(rest);
        }
    }
    None
}

fn prop_getter(p: &Proplist) -> impl Fn(&str) -> Option<String> + '_ {
    move |k| p.get_str(k)
}

fn convert_sink_state(s: PaSinkState) -> SinkState {
    match s {
        PaSinkState::Running => SinkState::Running,
        PaSinkState::Idle => SinkState::Idle,
        PaSinkState::Suspended => SinkState::Suspended,
        PaSinkState::Invalid => SinkState::Unknown,
    }
}

// ---------------------------------------------------------------------------
// Blocking implementation
// ---------------------------------------------------------------------------

// Field order matters: the context must be dropped before its mainloop.
struct Conn {
    context: Context,
    mainloop: Mainloop,
    deadline: Instant,
}

impl Conn {
    fn open(deadline: Instant) -> Result<Conn, PulseError> {
        let mainloop = Mainloop::new()
            .ok_or_else(|| PulseError::Connect("mainloop creation failed".into()))?;
        let mut context = Context::new(&mainloop, "BazzitePods")
            .ok_or_else(|| PulseError::Connect("context creation failed".into()))?;
        context
            .connect(None, ContextFlagSet::NOAUTOSPAWN, None)
            .map_err(|e| PulseError::Connect(format!("{e}")))?;
        let mut conn = Conn {
            mainloop,
            context,
            deadline,
        };
        conn.run_until("connect", |c| match c.get_state() {
            ContextState::Ready => Some(Ok(())),
            ContextState::Failed | ContextState::Terminated => {
                Some(Err(PulseError::Connect("connection refused".into())))
            }
            _ => None,
        })?;
        Ok(conn)
    }

    /// Runs the mainloop (blocking poll, bounded by the deadline) until `check`
    /// returns `Some`.
    fn run_until<T>(
        &mut self,
        what: &'static str,
        mut check: impl FnMut(&Context) -> Option<Result<T, PulseError>>,
    ) -> Result<T, PulseError> {
        loop {
            if let Some(r) = check(&self.context) {
                return r;
            }
            if matches!(
                self.context.get_state(),
                ContextState::Failed | ContextState::Terminated
            ) {
                return Err(PulseError::Failed(format!("connection lost during {what}")));
            }
            let now = Instant::now();
            if now >= self.deadline {
                return Err(PulseError::Timeout(what));
            }
            let remaining = (self.deadline - now).as_micros().min(i32::MAX as u128) as u64;
            self.mainloop
                .prepare(Some(MicroSeconds(remaining.max(1))))
                .map_err(|e| PulseError::Failed(format!("mainloop prepare: {e}")))?;
            self.mainloop
                .poll()
                .map_err(|e| PulseError::Failed(format!("mainloop poll: {e}")))?;
            self.mainloop
                .dispatch()
                .map_err(|e| PulseError::Failed(format!("mainloop dispatch: {e}")))?;
        }
    }

    fn wait_ops<F: ?Sized>(
        &mut self,
        what: &'static str,
        ops: &[&Operation<F>],
    ) -> Result<(), PulseError> {
        self.run_until(what, |_| {
            if ops
                .iter()
                .all(|op| op.get_state() != OperationState::Running)
            {
                Some(Ok(()))
            } else {
                None
            }
        })
    }

    fn wait_success(
        &mut self,
        what: &'static str,
        result: &Rc<Cell<Option<bool>>>,
    ) -> Result<(), PulseError> {
        self.run_until(what, |_| match result.get() {
            Some(true) => Some(Ok(())),
            Some(false) => Some(Err(PulseError::Failed(what.to_string()))),
            None => None,
        })
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.context.disconnect();
    }
}

fn success_slot() -> (Rc<Cell<Option<bool>>>, Box<dyn FnMut(bool) + 'static>) {
    let slot = Rc::new(Cell::new(None));
    let s = slot.clone();
    (slot, Box::new(move |ok| s.set(Some(ok))))
}

pub fn snapshot_sync(timeout: Duration) -> Result<PulseSnapshot, PulseError> {
    let mut conn = Conn::open(Instant::now() + timeout)?;
    let introspect = conn.context.introspect();

    let cards: Rc<RefCell<Vec<CardInfo>>> = Rc::default();
    let sinks: Rc<RefCell<Vec<SinkInfo>>> = Rc::default();
    let inputs: Rc<RefCell<Vec<SinkInputInfo>>> = Rc::default();
    let sources: Rc<RefCell<Vec<SourceInfo>>> = Rc::default();
    let outputs: Rc<RefCell<Vec<SourceOutputInfo>>> = Rc::default();
    let default_sink: Rc<RefCell<Option<String>>> = Rc::default();
    let server_done = Rc::new(Cell::new(false));

    let c = cards.clone();
    let op_cards = introspect.get_card_info_list(move |r| {
        if let ListResult::Item(item) = r {
            let name = item.name.as_deref().unwrap_or("").to_string();
            let mac = bluez_mac_from(&name, prop_getter(&item.proplist));
            c.borrow_mut().push(CardInfo {
                index: item.index,
                mac,
                profiles: item
                    .profiles
                    .iter()
                    .filter_map(|p| {
                        Some(ProfileInfo {
                            name: p.name.as_deref()?.to_string(),
                            available: p.available,
                            priority: p.priority,
                        })
                    })
                    .collect(),
                active_profile: item
                    .active_profile
                    .as_ref()
                    .and_then(|p| p.name.as_deref().map(str::to_string)),
                name,
            });
        }
    });
    let s = sinks.clone();
    let op_sinks = introspect.get_sink_info_list(move |r| {
        if let ListResult::Item(item) = r {
            let name = item.name.as_deref().unwrap_or("").to_string();
            let mac = bluez_mac_from(&name, prop_getter(&item.proplist));
            s.borrow_mut().push(SinkInfo {
                index: item.index,
                card: item.card,
                mac,
                state: convert_sink_state(item.state),
                codec: item.proplist.get_str("api.bluez5.codec"),
                name,
            });
        }
    });
    let i = inputs.clone();
    let op_inputs = introspect.get_sink_input_info_list(move |r| {
        if let ListResult::Item(item) = r {
            i.borrow_mut().push(SinkInputInfo {
                index: item.index,
                sink: item.sink,
                corked: item.corked,
                app: item.proplist.get_str("application.name"),
                role: item.proplist.get_str("media.role"),
                muted: item.mute,
            });
        }
    });
    let so = sources.clone();
    let op_sources = introspect.get_source_info_list(move |r| {
        if let ListResult::Item(item) = r {
            so.borrow_mut().push(SourceInfo {
                index: item.index,
                card: item.card,
                is_monitor: item.monitor_of_sink.is_some(),
            });
        }
    });
    let o = outputs.clone();
    let op_outputs = introspect.get_source_output_info_list(move |r| {
        if let ListResult::Item(item) = r {
            o.borrow_mut().push(SourceOutputInfo {
                index: item.index,
                source: item.source,
                corked: item.corked,
            });
        }
    });
    let d = default_sink.clone();
    let done = server_done.clone();
    let op_server = introspect.get_server_info(move |info| {
        *d.borrow_mut() = info.default_sink_name.as_deref().map(str::to_string);
        done.set(true);
    });

    conn.wait_ops("list cards", &[&op_cards])?;
    conn.wait_ops("list sinks", &[&op_sinks])?;
    conn.wait_ops("list sink inputs", &[&op_inputs])?;
    conn.wait_ops("list sources", &[&op_sources])?;
    conn.wait_ops("list source outputs", &[&op_outputs])?;
    conn.wait_ops("server info", &[&op_server])?;

    let snapshot = PulseSnapshot {
        cards: cards.take(),
        sinks: sinks.take(),
        sink_inputs: inputs.take(),
        sources: sources.take(),
        source_outputs: outputs.take(),
        default_sink: default_sink.take(),
    };
    Ok(snapshot)
}

pub fn set_card_profile_sync(
    card_index: u32,
    profile: &str,
    timeout: Duration,
) -> Result<(), PulseError> {
    let mut conn = Conn::open(Instant::now() + timeout)?;
    let (slot, cb) = success_slot();
    let mut introspect = conn.context.introspect();
    let _op = introspect.set_card_profile_by_index(card_index, profile, Some(cb));
    conn.wait_success("set card profile", &slot)
}

pub fn set_default_sink_sync(name: &str, timeout: Duration) -> Result<(), PulseError> {
    let mut conn = Conn::open(Instant::now() + timeout)?;
    let (slot, mut cb) = success_slot();
    let _op = conn.context.set_default_sink(name, move |ok| cb(ok));
    conn.wait_success("set default sink", &slot)
}

pub fn move_sink_input_sync(
    input_index: u32,
    sink_index: u32,
    timeout: Duration,
) -> Result<(), PulseError> {
    let mut conn = Conn::open(Instant::now() + timeout)?;
    let (slot, cb) = success_slot();
    let mut introspect = conn.context.introspect();
    let _op = introspect.move_sink_input_by_index(input_index, sink_index, Some(cb));
    conn.wait_success("move sink input", &slot)
}

pub fn set_sink_input_mute_sync(
    input_index: u32,
    mute: bool,
    timeout: Duration,
) -> Result<(), PulseError> {
    let mut conn = Conn::open(Instant::now() + timeout)?;
    let (slot, cb) = success_slot();
    let mut introspect = conn.context.introspect();
    let _op = introspect.set_sink_input_mute(input_index, mute, Some(cb));
    conn.wait_success("mute sink input", &slot)
}

fn get_sink_volume(conn: &mut Conn, sink_name: &str) -> Result<ChannelVolumes, PulseError> {
    let introspect = conn.context.introspect();
    let volume: Rc<Cell<Option<ChannelVolumes>>> = Rc::default();
    let v = volume.clone();
    let op = introspect.get_sink_info_by_name(sink_name, move |r| {
        if let ListResult::Item(item) = r {
            v.set(Some(item.volume));
        }
    });
    conn.wait_ops("get sink volume", &[&op])?;
    volume
        .get()
        .ok_or_else(|| PulseError::Failed(format!("sink {sink_name} not found")))
}

/// Average volume of a sink in percent.
pub fn sink_volume_percent_sync(sink_name: &str, timeout: Duration) -> Result<u32, PulseError> {
    let mut conn = Conn::open(Instant::now() + timeout)?;
    let volume = get_sink_volume(&mut conn, sink_name)?;
    let channels = volume.len();
    if channels == 0 {
        return Err(PulseError::Failed("sink has no channels".into()));
    }
    let total: f64 = volume.get().iter().map(|v| v.0 as f64).sum();
    Ok(((total / channels as f64 / Volume::NORMAL.0 as f64) * 100.0).round() as u32)
}

/// Sets every channel of a sink to `percent`.
pub fn set_sink_volume_percent_sync(
    sink_name: &str,
    percent: u32,
    timeout: Duration,
) -> Result<(), PulseError> {
    let mut conn = Conn::open(Instant::now() + timeout)?;
    let current = get_sink_volume(&mut conn, sink_name)?;
    let raw = ((percent as f64 / 100.0) * Volume::NORMAL.0 as f64).round() as u32;
    let mut new_volumes = ChannelVolumes::default();
    new_volumes.set(current.len(), Volume(raw));
    let (slot, cb) = success_slot();
    let mut introspect = conn.context.introspect();
    let _op = introspect.set_sink_volume_by_name(sink_name, &new_volumes, Some(cb));
    conn.wait_success("set sink volume", &slot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playing_streams_skip_background_and_system_sounds() {
        let input = |index, corked, app: &str, role: Option<&str>| SinkInputInfo {
            index,
            sink: 1,
            corked,
            app: Some(app.into()),
            role: role.map(Into::into),
            muted: false,
        };
        let snap = PulseSnapshot {
            sink_inputs: vec![
                input(1, false, "Plex", Some("video")),
                input(2, true, "Brave", Some("music")),
                input(3, false, "easyeffects", None),
                input(4, false, "WEBRTC VoiceEngine", Some("phone")),
                input(5, false, "plasmashell", Some("event")),
            ],
            ..Default::default()
        };
        assert_eq!(snap.playing_streams(), vec![1]);
    }

    #[test]
    fn normalize_mac_variants() {
        assert_eq!(
            normalize_mac("aa:bb:cc:dd:ee:ff").as_deref(),
            Some("AA:BB:CC:DD:EE:FF")
        );
        assert_eq!(
            normalize_mac("AA_BB_CC_DD_EE_FF.1").as_deref(),
            Some("AA:BB:CC:DD:EE:FF")
        );
        assert_eq!(normalize_mac("AA:BB:CC"), None);
        assert_eq!(normalize_mac("ZZ:BB:CC:DD:EE:FF"), None);
    }

    #[test]
    fn mac_from_names_and_props() {
        let none = |_: &str| None;
        assert_eq!(
            bluez_mac_from("bluez_card.a0_b1_c2_d3_e4_f5", none).as_deref(),
            Some("A0:B1:C2:D3:E4:F5")
        );
        assert_eq!(
            bluez_mac_from("bluez_output.A0_B1_C2_D3_E4_F5.1", none).as_deref(),
            Some("A0:B1:C2:D3:E4:F5")
        );
        assert_eq!(bluez_mac_from("alsa_output.pci-0000_00_1f.3", none), None);
        let props = |k: &str| {
            (k == "api.bluez5.path").then(|| "/org/bluez/hci0/dev_A0_B1_C2_D3_E4_F5".to_string())
        };
        assert_eq!(
            bluez_mac_from("whatever", props).as_deref(),
            Some("A0:B1:C2:D3:E4:F5")
        );
    }

    #[test]
    fn recording_ignores_monitors_and_corked() {
        let snap = PulseSnapshot {
            sources: vec![
                SourceInfo {
                    index: 1,
                    card: Some(7),
                    is_monitor: true,
                },
                SourceInfo {
                    index: 2,
                    card: Some(7),
                    is_monitor: false,
                },
            ],
            source_outputs: vec![SourceOutputInfo {
                index: 10,
                source: 1,
                corked: false,
            }],
            ..Default::default()
        };
        assert!(!snap.is_recording_from_card(7));
        let mut s2 = snap.clone();
        s2.source_outputs.push(SourceOutputInfo {
            index: 11,
            source: 2,
            corked: true,
        });
        assert!(!s2.is_recording_from_card(7));
        s2.source_outputs.push(SourceOutputInfo {
            index: 12,
            source: 2,
            corked: false,
        });
        assert!(s2.is_recording_from_card(7));
        assert!(!s2.is_recording_from_card(8));
    }
}
