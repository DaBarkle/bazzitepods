//! Whether to take the AirPods back from another device: one pure decision, made on every
//! route tick, so a message the iPhone never sends can only delay it, never block it.
//!
//! The PC is home. The iPhone may have the AirPods while it plays media or has a call;
//! once it goes quiet they come back here. A notification that borrows them while the PC
//! is in use is undone after a moment, without waiting for the iPhone to let go.

use std::time::Duration;

/// How much this PC needs the AirPods right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Need {
    Nothing,
    /// A game, a player, a browser: any app playing here.
    Playing,
    /// A microphone is in use or a voice chat is open (Discord).
    Voice,
}

/// What the route watcher knows, as of this tick.
#[derive(Debug, Clone, Copy)]
pub(super) struct Inputs {
    /// `Some(false)`: another device has the AirPods.
    pub owns: Option<bool>,
    /// At least one AirPod is in an ear.
    pub worn: bool,
    /// The other device has a phone call on them.
    pub call: bool,
    /// The other device says it is playing media.
    pub peer_streaming: bool,
    /// How long the other device has been quiet (said it stopped, or the AirPods report
    /// nobody playing, or it left), if it is.
    pub peer_quiet_for: Option<Duration>,
    /// How long since another device took them, and whether only for a notification.
    pub lost_for: Option<Duration>,
    pub lost_to_borrow: bool,
    pub need: Need,
    /// Something was just started here (a press of play).
    pub fresh_play: bool,
    /// We stopped taking them back for a while (see `RouteState::backoff_until`).
    pub backing_off: bool,
    /// The "Bring my AirPods back to this PC" setting.
    pub return_home: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Verdict {
    /// This PC has them (or nobody does): nothing to decide.
    Ours,
    Wait(&'static str),
    /// `auto` when no press of play here asked for it, so it counts towards the back-off.
    Take {
        why: &'static str,
        auto: bool,
    },
}

/// A notification borrows the AirPods for its sound; take them back once it has played.
pub(super) const BORROW_TAKEBACK: Duration = Duration::from_secs(3);
/// The other device stopped playing: give it a moment in case the next video starts.
pub(super) const QUIET_TAKEBACK: Duration = Duration::from_secs(5);
/// The other device took them without playing anything (picked in Control Centre, or a
/// notification while nothing played here): return home if it still plays nothing.
pub(super) const IDLE_TAKEBACK: Duration = Duration::from_secs(10);

pub(super) fn decide(i: &Inputs) -> Verdict {
    if i.owns != Some(false) {
        return Verdict::Ours;
    }
    if i.call {
        return Verdict::Wait("the other device has a call");
    }
    if !i.worn {
        return Verdict::Wait("the AirPods aren't in your ears");
    }
    if i.fresh_play {
        return Verdict::Take {
            why: "something started playing here",
            auto: false,
        };
    }
    if i.backing_off {
        return Verdict::Wait("they were moved back to the other device; leaving them there");
    }
    let quiet = i.peer_quiet_for;
    if i.peer_streaming && quiet.is_none() {
        return Verdict::Wait("the other device is playing");
    }
    let lost_for = i.lost_for.unwrap_or(Duration::MAX);
    if i.lost_to_borrow && lost_for >= BORROW_TAKEBACK && i.need >= Need::Playing {
        return Verdict::Take {
            why: "a notification borrowed them while this PC was using them",
            auto: true,
        };
    }
    if quiet.is_some_and(|q| q >= QUIET_TAKEBACK) {
        if i.need >= Need::Playing {
            return Verdict::Take {
                why: "the other device went quiet and this PC is using audio",
                auto: true,
            };
        }
        if i.return_home {
            return Verdict::Take {
                why: "the other device went quiet; bringing them home",
                auto: true,
            };
        }
    }
    if i.lost_to_borrow && lost_for >= IDLE_TAKEBACK && i.return_home {
        return Verdict::Take {
            why: "the other device played nothing; bringing them home",
            auto: true,
        };
    }
    if i.return_home || i.need >= Need::Playing {
        Verdict::Wait("waiting for the other device to go quiet")
    } else {
        Verdict::Wait("returning home is off and nothing plays here")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    /// The iPhone has the AirPods, they are worn, nothing else is known.
    fn base() -> Inputs {
        Inputs {
            owns: Some(false),
            worn: true,
            call: false,
            peer_streaming: false,
            peer_quiet_for: None,
            lost_for: Some(S(1)),
            lost_to_borrow: false,
            need: Need::Nothing,
            fresh_play: false,
            backing_off: false,
            return_home: true,
        }
    }

    fn takes(i: &Inputs) -> bool {
        matches!(decide(i), Verdict::Take { .. })
    }

    #[test]
    fn nothing_to_do_while_the_pc_has_them() {
        for owns in [Some(true), None] {
            let i = Inputs { owns, ..base() };
            assert_eq!(decide(&i), Verdict::Ours);
        }
    }

    #[test]
    fn a_notification_while_gaming_is_undone_after_its_sound() {
        // The bug (2026-10-28): a game's stream never "starts", so nothing took them back.
        let mut i = Inputs {
            lost_to_borrow: true,
            need: Need::Playing,
            lost_for: Some(S(1)),
            ..base()
        };
        assert!(!takes(&i));
        i.lost_for = Some(BORROW_TAKEBACK);
        assert!(takes(&i));
        // Same in a Discord call, with the setting off.
        i.need = Need::Voice;
        i.return_home = false;
        assert!(takes(&i));
    }

    #[test]
    fn the_iphone_playing_keeps_them_until_it_goes_quiet() {
        let mut i = Inputs {
            peer_streaming: true,
            lost_for: Some(S(600)),
            need: Need::Voice,
            ..base()
        };
        assert_eq!(decide(&i), Verdict::Wait("the other device is playing"));
        // Stopped: a few seconds' grace for the next video, then home.
        i.peer_quiet_for = Some(S(2));
        assert!(!takes(&i));
        i.peer_quiet_for = Some(QUIET_TAKEBACK);
        assert!(takes(&i));
    }

    #[test]
    fn home_after_a_phone_video_even_with_nothing_playing_here() {
        // The downstairs case: game stopped, phone video watched and stopped.
        let i = Inputs {
            peer_quiet_for: Some(QUIET_TAKEBACK),
            ..base()
        };
        assert!(takes(&i));
        // With returning home switched off they stay, as on a Mac...
        let off = Inputs {
            return_home: false,
            ..i
        };
        assert!(!takes(&off));
        // ...unless the PC is using audio.
        let busy = Inputs {
            need: Need::Playing,
            ..off
        };
        assert!(takes(&busy));
    }

    #[test]
    fn picked_on_the_iphone_but_never_played_returns_home_later() {
        let mut i = Inputs {
            lost_to_borrow: true,
            lost_for: Some(S(5)),
            ..base()
        };
        assert!(!takes(&i));
        i.lost_for = Some(IDLE_TAKEBACK);
        assert!(takes(&i));
        i.return_home = false;
        assert!(!takes(&i));
    }

    #[test]
    fn never_during_a_call_or_out_of_the_ears() {
        let busy = Inputs {
            lost_to_borrow: true,
            lost_for: Some(S(60)),
            peer_quiet_for: Some(S(60)),
            need: Need::Voice,
            fresh_play: true,
            ..base()
        };
        assert!(takes(&busy));
        assert!(!takes(&Inputs { call: true, ..busy }));
        assert!(!takes(&Inputs {
            worn: false,
            ..busy
        }));
    }

    #[test]
    fn backing_off_stops_automatic_takes_but_not_a_press_of_play() {
        let i = Inputs {
            lost_to_borrow: true,
            lost_for: Some(S(60)),
            peer_quiet_for: Some(S(60)),
            need: Need::Playing,
            backing_off: true,
            ..base()
        };
        assert!(!takes(&i));
        let pressed = Inputs {
            fresh_play: true,
            ..i
        };
        assert_eq!(
            decide(&pressed),
            Verdict::Take {
                why: "something started playing here",
                auto: false
            }
        );
    }

    #[test]
    fn a_stale_playing_report_gives_way_once_the_airpods_say_nobody_plays() {
        let i = Inputs {
            peer_streaming: true,
            peer_quiet_for: Some(QUIET_TAKEBACK),
            ..base()
        };
        assert!(takes(&i));
    }
}
