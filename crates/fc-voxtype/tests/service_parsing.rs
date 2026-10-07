//! `service.rs` parsing, against `systemctl --user show` text. The
//! active/not-found fixtures are real captures from this machine (voxtype.service
//! is installed and running here); "inactive-but-installed" is hand-written,
//! since stopping the user's real dictation daemon to capture it is exactly
//! what the task constraints forbid.

use fc_voxtype::service::{parse_show, ServiceStatus};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap_or_else(|e| panic!("reading fixture {name}: {e}"))
}

#[test]
fn active_real_capture() {
    let status = parse_show(&fixture("systemctl_show_active.txt"));
    assert_eq!(
        status,
        ServiceStatus::Found {
            active_state: "active".into(),
            sub_state: "running".into(),
            unit_file_state: "enabled".into(),
        }
    );
    assert!(status.is_active());
}

#[test]
fn not_found_real_capture() {
    // Captured against `systemctl --user show ... does-not-exist.service`:
    // exits 0, ActiveState=inactive, SubState=dead, UnitFileState=<empty>.
    let status = parse_show(&fixture("systemctl_show_not_found.txt"));
    assert_eq!(status, ServiceStatus::NotInstalled);
    assert!(!status.is_active());
}

#[test]
fn inactive_but_installed() {
    let status = parse_show(&fixture("systemctl_show_inactive.txt"));
    assert_eq!(
        status,
        ServiceStatus::Found {
            active_state: "inactive".into(),
            sub_state: "dead".into(),
            unit_file_state: "disabled".into(),
        }
    );
    assert!(!status.is_active());
}

#[test]
fn tolerates_reordered_and_extra_lines() {
    let text =
        "SubState=running\nSomeOtherProperty=whatever\nActiveState=active\nUnitFileState=enabled\n";
    let status = parse_show(text);
    assert!(status.is_active());
}
