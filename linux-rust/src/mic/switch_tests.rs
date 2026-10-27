use super::*;
use crate::audio::pulse::{MicRecording, MicSource, ModuleEntry};
use std::sync::Mutex;

#[derive(Default)]
struct World {
    log: Vec<String>,
    snap: MicSnapshot,
    load_fails: bool,
    ca: Option<bool>,
    packets: Option<mpsc::Sender<Vec<u8>>>,
    reports: Vec<MicReport>,
}

type Shared = Arc<Mutex<World>>;

fn lock(w: &Shared) -> std::sync::MutexGuard<'_, World> {
    w.lock().unwrap_or_else(|p| p.into_inner())
}

struct Host(Shared);
struct Link(Shared);

const OURS: u32 = 77;

impl MicHost for Host {
    async fn snapshot(&self) -> Result<MicSnapshot, String> {
        Ok(lock(&self.0).snap.clone())
    }

    async fn load_virtual_mic(&self, _fifo: &Path) -> Result<u32, String> {
        let mut w = lock(&self.0);
        w.log.push("load".into());
        if w.load_fails {
            return Err("refused".into());
        }
        w.snap.sources.push(MicSource {
            index: OURS,
            name: SOURCE_NAME.into(),
            is_monitor: false,
        });
        w.snap.modules.push(ModuleEntry {
            index: 500,
            name: "module-pipe-source".into(),
            argument: format!("source_name={SOURCE_NAME}"),
        });
        Ok(500)
    }

    async fn unload(&self, module: u32) -> Result<(), String> {
        let mut w = lock(&self.0);
        w.log.push(format!("unload:{module}"));
        w.snap.modules.retain(|m| m.index != module);
        w.snap.sources.retain(|s| s.index != OURS);
        Ok(())
    }

    async fn set_default(&self, source: &str) -> Result<(), String> {
        let mut w = lock(&self.0);
        w.log.push(format!("default:{source}"));
        w.snap.default_source = Some(source.into());
        // Apps that follow the default move with it; pinned ones (Discord here) don't.
        let to = w.snap.source(source).map(|s| s.index);
        if let Some(to) = to {
            for r in w
                .snap
                .recordings
                .iter_mut()
                .filter(|r| r.app.as_deref() != Some("Discord"))
            {
                r.source = to;
            }
        }
        Ok(())
    }

    async fn move_recording(&self, recording: u32, source: &str) -> Result<(), String> {
        let mut w = lock(&self.0);
        w.log.push(format!("move:{recording}->{source}"));
        let to = w.snap.source(source).map(|s| s.index);
        if let Some(to) = to {
            for r in w
                .snap
                .recordings
                .iter_mut()
                .filter(|r| r.index == recording)
            {
                r.source = to;
            }
        }
        Ok(())
    }

    fn start_decoder(
        &self,
        mut packets: mpsc::Receiver<Vec<u8>>,
        _fifo: &Path,
        live: Liveness,
    ) -> Result<(), String> {
        lock(&self.0).log.push("decoder".into());
        tokio::spawn(async move {
            while packets.recv().await.is_some() {
                live.mark();
            }
        });
        Ok(())
    }
}

impl MicLink for Link {
    async fn start_stream(&self, packets: mpsc::Sender<Vec<u8>>) -> Result<(), String> {
        let mut w = lock(&self.0);
        w.log.push("start".into());
        w.packets = Some(packets);
        Ok(())
    }

    async fn stop_stream(&self) {
        let mut w = lock(&self.0);
        w.log.push("stop".into());
        w.packets = None;
    }

    async fn conversation_awareness(&self) -> Option<bool> {
        lock(&self.0).ca
    }

    async fn set_conversation_awareness(&self, on: bool) {
        let mut w = lock(&self.0);
        w.log.push(format!("ca:{on}"));
        w.ca = Some(on);
    }

    fn rebuild_output(&self) {
        lock(&self.0).log.push("rebuild".into());
    }
}

fn desk_world() -> World {
    World {
        ca: Some(true),
        snap: MicSnapshot {
            default_source: Some("yeti".into()),
            sources: vec![
                MicSource {
                    index: 3,
                    name: "yeti".into(),
                    is_monitor: false,
                },
                MicSource {
                    index: 4,
                    name: "speakers.monitor".into(),
                    is_monitor: true,
                },
            ],
            recordings: vec![
                MicRecording {
                    index: 40,
                    source: 3,
                    app: Some("easyeffects".into()),
                },
                MicRecording {
                    index: 41,
                    source: 3,
                    app: Some("Discord".into()),
                },
            ],
            modules: Vec::new(),
        },
        ..Default::default()
    }
}

struct Rig {
    world: Shared,
    handle: MicSwitchHandle,
    dir: PathBuf,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn rig_with(world: World, watchdog: Duration, prepare: impl FnOnce(&SwitchConfig)) -> Rig {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("bp-switch-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = SwitchConfig {
        state_path: dir.join("mic_switch.json"),
        fifo: dir.join("mic.fifo"),
        settle: Duration::from_millis(50),
        watchdog,
    };
    prepare(&cfg);
    let world: Shared = Arc::new(Mutex::new(world));
    let w = world.clone();
    let report = move |r: MicReport| lock(&w).reports.push(r);
    let handle = spawn(
        Host(world.clone()),
        Link(world.clone()),
        cfg,
        Arc::new(report),
    );
    Rig { world, handle, dir }
}

fn rig(world: World) -> Rig {
    rig_with(world, Duration::from_secs(60), |_| {})
}

async fn settle(ms: u64) {
    sleep(Duration::from_millis(ms)).await;
}

fn toggle() -> MicRequest {
    MicRequest::Toggle { allowed: Ok(()) }
}

fn log(rig: &Rig) -> Vec<String> {
    lock(&rig.world).log.clone()
}

fn last_report(rig: &Rig) -> Option<MicReport> {
    lock(&rig.world).reports.last().cloned()
}

#[tokio::test]
async fn left_hold_switches_to_the_airpods_mic_and_back() {
    let rig = rig(desk_world());
    rig.handle.request(toggle());
    settle(200).await;
    assert_eq!(
        log(&rig),
        [
            "load",
            "decoder",
            "start",
            "default:bazzitepods_airpods_mic",
            "ca:false",
            "move:41->bazzitepods_airpods_mic",
        ]
    );
    assert_eq!(
        last_report(&rig),
        Some(MicReport {
            route: MicRoute::AirPods,
            problem: None
        })
    );
    // While it runs, a crash would be undone from this file.
    let saved = load_saved(&rig.dir.join("mic_switch.json")).unwrap();
    assert_eq!(saved.desk_source.as_deref(), Some("yeti"));
    assert!(saved.restore_conversation_awareness);

    lock(&rig.world).log.clear();
    rig.handle.request(toggle());
    settle(100).await;
    assert_eq!(
        log(&rig),
        [
            "default:yeti",
            "move:41->yeti",
            "stop",
            "unload:500",
            "ca:true",
            "rebuild",
        ]
    );
    assert_eq!(last_report(&rig).map(|r| r.route), Some(MicRoute::Desk));
    assert!(!rig.dir.join("mic_switch.json").exists());
    let w = lock(&rig.world);
    assert!(w.snap.recordings.iter().all(|r| r.source == 3));
}

#[tokio::test]
async fn conversation_awareness_that_was_off_stays_off() {
    let mut world = desk_world();
    world.ca = Some(false);
    let rig = rig(world);
    rig.handle.request(toggle());
    settle(150).await;
    rig.handle.request(toggle());
    settle(100).await;
    assert!(!log(&rig).iter().any(|l| l.starts_with("ca:")));
}

#[tokio::test]
async fn not_allowed_reports_why_and_touches_nothing() {
    let rig = rig(desk_world());
    rig.handle.request(MicRequest::Toggle {
        allowed: Err("The AirPods are in use on another device"),
    });
    settle(50).await;
    assert!(log(&rig).is_empty());
    let r = last_report(&rig).unwrap();
    assert_eq!(r.route, MicRoute::Desk);
    assert_eq!(
        r.problem.as_deref(),
        Some("The AirPods are in use on another device")
    );
}

#[tokio::test]
async fn a_refused_virtual_mic_leaves_everything_as_it_was() {
    let mut world = desk_world();
    world.load_fails = true;
    let rig = rig(world);
    rig.handle.request(toggle());
    settle(100).await;
    assert_eq!(log(&rig), ["load"]);
    let r = last_report(&rig).unwrap();
    assert_eq!(r.route, MicRoute::Desk);
    assert!(r.problem.is_some());
    assert!(!rig.dir.join("mic_switch.json").exists());
}

#[tokio::test]
async fn desk_request_and_shutdown_go_back_once() {
    let rig = rig(desk_world());
    rig.handle.request(MicRequest::Desk("nothing to do"));
    settle(50).await;
    assert!(log(&rig).is_empty());
    rig.handle.request(toggle());
    settle(150).await;
    lock(&rig.world).log.clear();
    rig.handle
        .request(MicRequest::Desk("the AirPods went in the case"));
    rig.handle.shutdown().await;
    let l = log(&rig);
    assert_eq!(l.iter().filter(|x| *x == "stop").count(), 1, "{l:?}");
    assert_eq!(l.iter().filter(|x| *x == "rebuild").count(), 1, "{l:?}");
}

#[tokio::test]
async fn a_stream_that_stops_on_its_own_is_not_restarted() {
    let rig = rig_with(desk_world(), Duration::from_millis(300), |_| {});
    rig.handle.request(toggle());
    settle(100).await;
    // Packets flow for a while...
    for _ in 0..6 {
        let tx = lock(&rig.world).packets.clone().unwrap();
        tx.send(vec![0]).await.unwrap();
        settle(100).await;
    }
    assert_eq!(last_report(&rig).map(|r| r.route), Some(MicRoute::AirPods));
    // ...then stop (the iPhone took the AirPods).
    settle(1200).await;
    let r = last_report(&rig).unwrap();
    assert_eq!(r.route, MicRoute::Desk);
    assert!(r.problem.is_some());
    let l = log(&rig);
    assert_eq!(l.iter().filter(|x| *x == "start").count(), 1, "{l:?}");
    assert!(l.contains(&"rebuild".to_string()));
}

#[tokio::test]
async fn recovers_from_a_crash_on_the_airpods_mic() {
    let mut world = desk_world();
    world.ca = Some(false);
    world.snap.default_source = Some(SOURCE_NAME.into());
    world.snap.modules.push(ModuleEntry {
        index: 12,
        name: "module-pipe-source".into(),
        argument: format!("source_name={SOURCE_NAME} file=/x"),
    });
    world.snap.modules.push(ModuleEntry {
        index: 13,
        name: "module-null-sink".into(),
        argument: "sink_name=other".into(),
    });
    let rig = rig_with(world, Duration::from_secs(60), |cfg| {
        save(
            &cfg.state_path,
            &Saved {
                desk_source: Some("yeti".into()),
                restore_conversation_awareness: true,
            },
        );
    });
    settle(100).await;
    assert_eq!(log(&rig), ["unload:12", "default:yeti", "ca:true"]);
    assert!(!rig.dir.join("mic_switch.json").exists());
}
