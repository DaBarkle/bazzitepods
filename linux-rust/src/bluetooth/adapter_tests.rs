use super::*;

fn c(name: &str, powered: bool, has_airpods: bool) -> Candidate {
    Candidate {
        name: name.into(),
        powered,
        has_airpods,
    }
}

#[test]
fn prefers_the_adapter_with_the_airpods_over_the_default() {
    let all = [c("hci0", true, false), c("hci1", true, true)];
    assert_eq!(pick(&all, Some("hci0")), Some("hci1"));
}

#[test]
fn prefers_a_powered_adapter_when_both_know_the_airpods() {
    let all = [c("hci0", false, true), c("hci1", true, true)];
    assert_eq!(pick(&all, Some("hci0")), Some("hci1"));
}

#[test]
fn an_unpowered_adapter_with_the_airpods_beats_one_without() {
    let all = [c("hci0", true, false), c("hci1", false, true)];
    assert_eq!(pick(&all, Some("hci0")), Some("hci1"));
}

#[test]
fn falls_back_to_the_default_then_the_first() {
    let all = [c("hci0", true, false), c("hci1", true, false)];
    assert_eq!(pick(&all, Some("hci1")), Some("hci1"));
    assert_eq!(pick(&all, None), Some("hci0"));
    assert_eq!(pick(&[], Some("hci0")), None);
}
