//! Unit tests for the shell's pure functions: option parsing,
//! resource-location order, and the port policy.

use std::path::{Path, PathBuf};

use crate::{
    options::{
        Env,
        Options,
        app_server_candidates,
        first_existing,
        gateway_port,
        web_dir_candidates,
    },
    window_nav::next_window_url,
};

#[test]
fn flags_parse_and_defaults_hold() {
    let options = Options::parse_from(
        &[
            "--directory".into(),
            "/tmp/ws".into(),
            "--profile".into(),
            "p.toml".into(),
        ],
        &Env {
            app_server: None,
            backend_port: None,
        },
    )
    .expect("parses");
    assert_eq!(options.directory, PathBuf::from("/tmp/ws"));
    assert_eq!(options.profile, Some(PathBuf::from("p.toml")));
    assert!(options.app_server.is_none());
}

#[test]
fn an_unknown_flag_is_rejected() {
    let error = Options::parse_from(
        &["--nope".into()],
        &Env {
            app_server: None,
            backend_port: None,
        },
    )
    .expect_err("rejected");
    assert!(error.contains("--nope"));
}

#[test]
fn the_app_server_resolution_order_is_flag_then_exe_dir_then_path() {
    let flag = PathBuf::from("/opt/server");
    let exe_dir = Some(PathBuf::from("/usr/lib/elysper"));
    let candidates = app_server_candidates(Some(&flag), exe_dir.as_deref());
    assert_eq!(candidates[0], flag);
    assert_eq!(
        candidates[1],
        PathBuf::from("/usr/lib/elysper/elysper-app-server")
    );
    assert_eq!(candidates[2], PathBuf::from("elysper-app-server"));
}

#[test]
fn the_web_dir_resolution_order_is_flag_then_resource_then_dev_checkout() {
    let flag = PathBuf::from("/tmp/dist");
    let resource = PathBuf::from("/usr/lib/elysper/resources/dist");
    let candidates = web_dir_candidates(Some(&flag), Some(&resource));
    assert_eq!(candidates[0], flag);
    assert_eq!(candidates[1], resource);
    assert!(candidates[2].ends_with("elysper-web/dist"));
}

#[test]
fn first_existing_picks_the_first_candidate_that_exists() {
    let candidates = vec![
        PathBuf::from("/a"),
        PathBuf::from("/b"),
        PathBuf::from("/c"),
    ];
    let hit =
        first_existing(&candidates, |p| p == Path::new("/b")).expect("some");
    assert_eq!(hit, &PathBuf::from("/b"));
}

#[test]
fn dev_binds_the_vite_proxy_port_and_release_binds_zero() {
    assert_eq!(gateway_port(true, None).unwrap(), 0);
    assert_eq!(gateway_port(false, None).unwrap(), 8080);
    assert_eq!(gateway_port(false, Some("8081")).unwrap(), 8081);
    assert!(gateway_port(false, Some("not-a-port")).is_err());
}

#[test]
fn a_ready_with_a_new_port_navigates_the_window() {
    let url = next_window_url(
        Some("http://127.0.0.1:8080/"),
        None,
        "http://127.0.0.1:8081/",
    );
    assert_eq!(url.as_deref(), Some("http://127.0.0.1:8081/"));
}

#[test]
fn a_ready_with_the_same_url_does_nothing() {
    assert_eq!(
        next_window_url(
            Some("http://127.0.0.1:8080/"),
            None,
            "http://127.0.0.1:8080/"
        ),
        None
    );
}

#[test]
fn a_dev_window_stays_on_the_dev_server() {
    // In a dev session the window sits on the Vite server; a sidecar
    // port change is absorbed by the proxy and the front-end's own
    // reconnect (design D2).
    assert_eq!(
        next_window_url(
            Some("http://localhost:5173/"),
            Some("http://localhost:5173/"),
            "http://127.0.0.1:8081/"
        ),
        None
    );
}

#[test]
fn a_first_ready_without_a_window_url_navigates() {
    assert_eq!(
        next_window_url(None, None, "http://127.0.0.1:8080/").as_deref(),
        Some("http://127.0.0.1:8080/")
    );
}
