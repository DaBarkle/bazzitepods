//! More BLE advert decoding and key handling: which pod is which, what is in the case, and
//! how adverts are matched to our AirPods without ever connecting.

use super::*;

fn header(product: u16, status: u8, pods: u8, flags_case: u8, lid: u8) -> Vec<u8> {
    let [hi, lo] = product.to_be_bytes();
    vec![
        0x07, 0x19, 0x01, hi, lo, status, pods, flags_case, lid, 0x00, 0x04,
    ]
}

const AIRPODS_4: u16 = 0x1920;
const AIRPODS_MAX_LIGHTNING: u16 = 0x0A20;

fn block(primary: u8, secondary: u8, case: u8) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[1] = primary;
    b[2] = secondary;
    b[3] = case;
    b
}

#[test]
fn clear_text_right_primary_swaps_levels_and_charging() {
    // Right primary (bit5 clear): primary nibble is the low one, its charge flag bit0.
    // Low nibble 3 (30%, right, charging), high nibble 9 (90%, left).
    let data = header(AIRPODS_4, 0x04, 0x93, 0x10 | 0x07, 0);
    let r = decode_advert(&data, None).unwrap();
    assert_eq!(r.right, Some((30, true)));
    assert_eq!(r.left, Some((90, false)));
    assert_eq!(r.case, Some((70, false)));
}

#[test]
fn clear_text_nibbles_above_nine_mean_full() {
    let data = header(AIRPODS_4, 0x20 | 0x04, 0xEA, 0x0A, 0);
    let r = decode_advert(&data, None).unwrap();
    assert_eq!(r.left, Some((100, false)));
    assert_eq!(r.right, Some((100, false)));
    assert_eq!(r.case, Some((100, false)));
}

#[test]
fn level_bytes_at_the_edges() {
    let data = header(AIRPODS_4, 0x20 | 0x04, 0, 0, 0);
    let r = decode_proximity(&data, Some(&block(0, 100, 0x80 | 100))).unwrap();
    assert_eq!(r.left, Some((0, false)));
    assert_eq!(r.right, Some((100, false)));
    assert_eq!(r.case, Some((100, true)));
}

#[test]
fn which_pod_is_in_the_case() {
    let d = block(50, 60, 70);
    // Both in case.
    let r = decode_proximity(&header(AIRPODS_4, 0x20 | 0x04, 0, 0, 0), Some(&d)).unwrap();
    assert!(r.left_in_case && r.right_in_case);
    assert_eq!(r.case, Some((70, false)));

    // One in case (bit4), left primary not in case (bit6 clear), left in ear: so right
    // is the one in the case. xor = 1 -> left in ear is bit1.
    let r = decode_proximity(&header(AIRPODS_4, 0x20 | 0x10 | 0x02, 0, 0, 0), Some(&d)).unwrap();
    assert_eq!(r.left_in_ear, Some(true));
    assert_eq!(r.right_in_ear, Some(false));
    assert!(!r.left_in_case && r.right_in_case);

    // One in case, neither pod in an ear: assume the primary (right here) is in it.
    let r = decode_proximity(&header(AIRPODS_4, 0x10, 0, 0, 0), Some(&d)).unwrap();
    assert!(r.right_in_case && !r.left_in_case);

    // Nothing in the case: no case level, no lid state.
    let r = decode_proximity(&header(AIRPODS_4, 0x20, 0, 0, 0), Some(&d)).unwrap();
    assert!(!r.left_in_case && !r.right_in_case);
    assert_eq!(r.case, None);
    assert_eq!(r.lid_open, None);
}

#[test]
fn airpods_max_clear_text_and_unknown_slots() {
    // Low nibble 0xF (unknown), high nibble 6 -> 60%, charging flag bit4 of byte 7.
    let data = header(AIRPODS_MAX_LIGHTNING, 0x20, 0x6F, 0x10, 0);
    let r = decode_advert(&data, None).unwrap();
    assert_eq!(r.headphone, Some((60, true)));
    assert!(r.left.is_none() && r.right.is_none() && r.case.is_none());

    // Every slot unknown.
    let r = decode_proximity(
        &header(AIRPODS_MAX_LIGHTNING, 0x20, 0, 0, 0),
        Some(&block(0x7F, 0xFF, 0x7F)),
    )
    .unwrap();
    assert_eq!(r.headphone, None);
}

#[test]
fn unknown_models_still_decode_as_earbuds() {
    let r = decode_proximity(&header(0xBEEF, 0x20, 0, 0, 0), Some(&block(40, 41, 0))).unwrap();
    assert_eq!(r.product_id, Some(0xBEEF));
    assert_eq!(r.left, Some((40, false)));
    assert_eq!(r.right, Some((41, false)));
}

// ---------------------------------------------------------------------------
// Manufacturer data handling
// ---------------------------------------------------------------------------

fn enc_keys() -> EncKeys {
    Arc::new(RwLock::new(HashMap::new()))
}

#[test]
fn manufacturer_data_from_other_companies_is_skipped() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let public: Address = "AA:BB:CC:DD:EE:FF".parse().unwrap();
    let mut data = HashMap::new();
    data.insert(0x0006u16, vec![0x07, 0x19, 0x01]);
    assert!(handle_manufacturer_data(&data, public, &enc_keys(), &tx));
    assert!(rx.try_recv().is_err());
}

#[test]
fn apple_adverts_are_forwarded_with_the_public_mac() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let public: Address = "AA:BB:CC:DD:EE:FF".parse().unwrap();
    let mut data = HashMap::new();
    data.insert(
        APPLE_COMPANY_ID,
        header(AIRPODS_4, 0x20 | 0x04, 0x55, 0x05, 0),
    );
    assert!(handle_manufacturer_data(&data, public, &enc_keys(), &tx));
    let (mac, reading) = rx.try_recv().unwrap();
    assert_eq!(mac, public);
    assert_eq!(reading.left, Some((50, false)));

    // A non-battery Apple advert (e.g. Handoff) is ignored but keeps the watch going.
    data.insert(APPLE_COMPANY_ID, vec![0x0C, 0x0E, 0x00]);
    assert!(handle_manufacturer_data(&data, public, &enc_keys(), &tx));
    assert!(rx.try_recv().is_err());

    // Nobody listening any more: stop watching.
    drop(rx);
    data.insert(APPLE_COMPANY_ID, header(AIRPODS_4, 0x20, 0x55, 0x05, 0));
    assert!(!handle_manufacturer_data(&data, public, &enc_keys(), &tx));
}

#[test]
fn monitor_api_missing_is_recognised() {
    let err = |kind, message: &str| bluer::Error {
        kind,
        message: message.into(),
    };
    assert!(monitor_unsupported(&err(
        bluer::ErrorKind::NotSupported,
        ""
    )));
    assert!(monitor_unsupported(&err(
        bluer::ErrorKind::Failed,
        "Method \"RegisterMonitor\" doesn't exist"
    )));
    assert!(monitor_unsupported(&err(
        bluer::ErrorKind::Failed,
        "org.freedesktop.DBus.Error.UnknownInterface"
    )));
    assert!(!monitor_unsupported(&err(
        bluer::ErrorKind::NotReady,
        "adapter is powered off"
    )));
}

// ---------------------------------------------------------------------------
// Key store
// ---------------------------------------------------------------------------

fn devices_json(mac: &str, irk: &str, enc: &str) -> String {
    format!(
        r#"{{"{mac}": {{"name":"x","type_":"AirPods","information":{{"kind":"AirPods","data":{{
            "name":"x","model_number":"A3053","manufacturer":"Apple","serial_number":"",
            "version1":"","version2":"","hardware_revision":"","updater_identifier":"",
            "left_serial_number":"","right_serial_number":"","version3":"",
            "le_keys":{{"irk":"{irk}","enc_key":"{enc}"}}}}}}}}}}"#
    )
}

fn rpa_for(irk: &[u8; 16]) -> Address {
    let prand = [0x11, 0x22, 0x40 | 0x33];
    let hash = ah(irk, &prand);
    Address([prand[2], prand[1], prand[0], hash[2], hash[1], hash[0]])
}

#[tokio::test]
async fn key_store_loads_devices_file_and_reloads_when_it_changes() {
    let dir = std::env::temp_dir().join(format!("librepods-le-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("devices.json");
    let irk = [0x5Au8; 16];
    let public: Address = "AA:BB:CC:DD:EE:FF".parse().unwrap();
    let rpa = rpa_for(&irk);
    assert!(verify_rpa(rpa, &irk));

    // No file yet: nothing matches.
    let mut ks = KeyStore::new(path.clone());
    ks.refresh_if_changed().await;
    assert_eq!(ks.resolve(rpa), None);

    // Keys appear after pairing. The store re-checks at most every couple of seconds.
    std::fs::write(
        &path,
        devices_json(
            &public.to_string(),
            &hex::encode(irk),
            &hex::encode([7u8; 16]),
        ),
    )
    .unwrap();
    ks.last_check = None;
    ks.refresh_if_changed().await;
    assert_eq!(ks.keys.len(), 1);
    assert_eq!(
        ks.resolve(rpa),
        Some(public),
        "negative cache cleared on reload"
    );
    assert_eq!(ks.resolve(rpa), Some(public), "cached");
    assert_eq!(enc_key_for(&ks.enc_keys, public), Some([7u8; 16]));

    // Within the reload interval the file is not even looked at.
    std::fs::write(&path, "{}").unwrap();
    ks.refresh_if_changed().await;
    assert_eq!(ks.keys.len(), 1);

    // A corrupt file is treated as "no keys", never a crash.
    std::fs::write(&path, "{ not json").unwrap();
    ks.last_check = None;
    ks.mtime = None;
    ks.refresh_if_changed().await;
    assert!(ks.keys.is_empty());
    assert_eq!(ks.resolve(rpa), None);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn bad_keys_in_devices_file_are_skipped() {
    let parse = |json: &str| -> HashMap<String, DeviceData> { serde_json::from_str(json).unwrap() };
    // IRK too short, IRK not hex, MAC unreadable: each entry is dropped.
    for (mac, irk) in [
        ("AA:BB:CC:DD:EE:FF", "0011"),
        ("AA:BB:CC:DD:EE:FF", "zz112233445566778899aabbccddeeff"),
        ("not-a-mac", "00112233445566778899aabbccddeeff"),
    ] {
        assert!(
            keys_from_devices(&parse(&devices_json(mac, irk, ""))).is_empty(),
            "{mac} {irk}"
        );
    }
    // Other brands of earbuds never take part in AirPods matching.
    let nothing =
        parse(r#"{"AA:BB:CC:DD:EE:FF":{"name":"Ear","type_":"Nothing","information":null}}"#);
    assert!(keys_from_devices(&nothing).is_empty());
    // A bad encryption key keeps the IRK but no decryption.
    let keys = keys_from_devices(&parse(&devices_json(
        "AA:BB:CC:DD:EE:FF",
        "00112233445566778899aabbccddeeff",
        "1234",
    )));
    assert_eq!(keys.len(), 1);
    assert!(keys[0].enc_key.is_none());
}

#[test]
fn negative_cache_is_bounded() {
    let mut ks = KeyStore::new(PathBuf::from("/nonexistent"));
    for i in 0..5000u32 {
        let b = i.to_be_bytes();
        ks.resolve(Address([0x40, 0, b[0], b[1], b[2], b[3]]));
    }
    assert!(ks.failed.len() <= 4097);
}
