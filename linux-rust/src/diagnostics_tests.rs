use super::*;

#[test]
fn reads_usb_ids_from_a_modalias() {
    assert_eq!(
        usb_ids("usb:v0BDAp8771d0200dcE0dsc01dp01icE0isc01ip01in00\n"),
        Some((0x0bda, 0x8771))
    );
    assert_eq!(usb_ids("pci:v00008086d00002723"), None);
    assert_eq!(usb_ids("usb:v0BD"), None);
}

#[test]
fn flags_the_rtl8761b_only() {
    assert!(known_problem(0x0bda, 0x8771).is_some());
    assert!(known_problem(0x2357, 0x0604).is_some());
    assert!(known_problem(0x8087, 0x0029).is_none()); // Intel AX200
}

#[test]
fn codec_note_explains_sbc() {
    assert_eq!(codec_note(Some("AAC")), "AAC");
    assert!(codec_note(Some("SBC-XQ")).contains("no AAC"));
    assert_eq!(codec_note(Some("Restoring audio")), "Restoring audio");
    assert!(codec_note(None).starts_with("Connect"));
}
