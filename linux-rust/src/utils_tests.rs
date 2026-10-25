//! The address-hash function used to recognise our AirPods' rotating BLE addresses.

use super::*;

/// Bluetooth Core Specification, Vol 3 Part H, Appendix D.7 ("ah" sample data), with the
/// spec's most-significant-first values stored least significant byte first.
#[test]
fn ah_matches_the_bluetooth_spec_sample() {
    let mut irk: [u8; 16] = hex::decode("ec0234a357c8ad05341010a60a397d9b")
        .unwrap()
        .try_into()
        .unwrap();
    irk.reverse();
    let prand = [0x94, 0x81, 0x70];
    assert_eq!(ah(&irk, &prand), [0xaa, 0xfb, 0x0d]);
}

#[test]
fn ah_depends_on_key_and_random_part() {
    let k = [1u8; 16];
    assert_ne!(ah(&k, &[1, 2, 3]), ah(&k, &[1, 2, 4]));
    assert_ne!(ah(&k, &[1, 2, 3]), ah(&[2u8; 16], &[1, 2, 3]));
    assert_eq!(ah(&k, &[1, 2, 3]), ah(&k, &[1, 2, 3]));
}
