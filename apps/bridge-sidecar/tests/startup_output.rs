use std::{
    fs,
    net::{Ipv4Addr, TcpListener},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn startup_output_never_emits_pairing_or_control_credentials() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("test listener binds");
    let bind_addr = listener.local_addr().expect("listener address resolves");
    let temp_dir = std::env::temp_dir().join(format!(
        "bridge-sidecar-startup-output-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time is valid")
            .as_nanos()
    ));
    fs::create_dir_all(&temp_dir).expect("temporary directory creates");
    let control_token = "local-control-secret-for-output-test";

    let output = Command::new(env!("CARGO_BIN_EXE_bridge-sidecar"))
        .env("CODEX_MOBILE_BRIDGE_BIND", bind_addr.to_string())
        .env("CODEX_MOBILE_BRIDGE_DEBUG_PORT", "1")
        .env("CODEX_MOBILE_BRIDGE_DB", temp_dir.join("bridge.sqlite"))
        .env("CODEX_MOBILE_BRIDGE_PWA_DIR", &temp_dir)
        .env("CODEX_MOBILE_BRIDGE_CONTROL_TOKEN", control_token)
        .output()
        .expect("sidecar runs until the occupied bind address stops startup");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = fs::remove_dir_all(&temp_dir);

    assert!(
        !output.status.success(),
        "occupied port must stop the sidecar"
    );
    assert!(combined.contains("Codex mobile bridge listening on"));
    assert!(combined.contains("Serving PWA assets"));
    assert!(!combined.contains(&temp_dir.display().to_string()));
    assert!(
        !combined.contains(control_token),
        "startup output exposed the Local Control Token:\n{combined}"
    );
    assert!(
        !combined.contains("pairingToken="),
        "startup output exposed a Pairing Token:\n{combined}"
    );
}
