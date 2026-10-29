use super::*;

fn bytes(states: &[u32]) -> Vec<u8> {
    states.iter().flat_map(|s| s.to_ne_bytes()).collect()
}

#[test]
fn reads_activated_and_fullscreen() {
    assert_eq!(parse_state(&bytes(&[])), (false, false));
    assert_eq!(parse_state(&bytes(&[0, 2])), (true, false));
    assert_eq!(parse_state(&bytes(&[3, 2])), (true, true));
    assert_eq!(parse_state(&bytes(&[3])), (false, true));
}

#[test]
fn only_an_active_fullscreen_window_counts() {
    assert!(!fullscreen_in_front(&[(true, false), (false, true)]));
    assert!(fullscreen_in_front(&[(false, false), (true, true)]));
    assert!(!fullscreen_in_front(&[]));
}
