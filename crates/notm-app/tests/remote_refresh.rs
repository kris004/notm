//! Hermetic real-process CLI and real-D-Bus protocol tests, without GTK.

use std::{
    process::{Command, Output},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use gio::glib::{self, variant::ToVariant};
use serde_json::Value;

#[path = "support/refresh_bus.rs"]
mod refresh_bus;
use refresh_bus::PrivateBus;

const NORMAL: &str = "io.github.kris004.notm";
const TEST_A: &str = "io.github.kris004.notm.test.a";
const TEST_B: &str = "io.github.kris004.notm.test.b";
const XML: &str = r#"<node><interface name="io.github.kris004.notm.SearchRefresh1">
<method name="Refresh"><arg type="u" direction="in"/><arg type="t" direction="out"/></method>
</interface></node>"#;

struct Peer {
    stop: mpsc::Sender<()>,
    worker: Option<thread::JoinHandle<()>>,
    calls: Arc<AtomicUsize>,
}

impl Peer {
    fn start(bus: &PrivateBus, name: &str, supported: bool, delay: Duration) -> Self {
        let (stop, stopped) = mpsc::channel();
        let (ready, started) = mpsc::sync_channel(1);
        let address = bus.address.clone();
        let name = name.to_owned();
        let calls = Arc::new(AtomicUsize::new(0));
        let received = calls.clone();
        let worker = thread::spawn(move || {
            let context = glib::MainContext::new();
            context
                .with_thread_default(|| {
                    let connection = gio::DBusConnection::for_address_sync(
                        &address,
                        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                            | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
                        None,
                        gio::Cancellable::NONE,
                    )
                    .unwrap();
                    connection.set_exit_on_close(false);
                    let registration = supported.then(|| {
                        let interface = gio::DBusNodeInfo::for_xml(XML)
                            .unwrap()
                            .lookup_interface("io.github.kris004.notm.SearchRefresh1")
                            .unwrap();
                        connection
                            .register_object("/io/github/kris004/notm/SearchRefresh", &interface)
                            .method_call(move |_, _, _, _, _, _, invocation| {
                                received.fetch_add(1, Ordering::SeqCst);
                                glib::MainContext::ref_thread_default().spawn_local(async move {
                                    glib::timeout_future(delay).await;
                                    invocation.return_value(Some(&(42u64,).to_variant()));
                                });
                            })
                            .build()
                            .unwrap()
                    });
                    connection
                        .call_sync(
                            Some("org.freedesktop.DBus"),
                            "/org/freedesktop/DBus",
                            "org.freedesktop.DBus",
                            "RequestName",
                            Some(&(name.as_str(), 0u32).to_variant()),
                            None,
                            gio::DBusCallFlags::NO_AUTO_START,
                            1000,
                            gio::Cancellable::NONE,
                        )
                        .unwrap();
                    ready.send(()).unwrap();
                    while stopped.try_recv().is_err() {
                        while context.pending() {
                            context.iteration(false);
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                    if let Some(id) = registration {
                        connection.unregister_object(id).unwrap();
                    }
                    connection.close_sync(gio::Cancellable::NONE).unwrap();
                })
                .unwrap();
        });
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        Self {
            stop,
            worker: Some(worker),
            calls,
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        self.worker.take().unwrap().join().unwrap();
    }
}

fn refresh(bus: &PrivateBus, args: &[&str]) -> Output {
    let home = tempfile::tempdir().unwrap();
    // Invalid config and no display must not affect a production control call.
    std::fs::create_dir_all(home.path().join("notm")).unwrap();
    std::fs::write(home.path().join("notm/config.toml"), "invalid = [").unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_notm"));
    bus.configure(&mut command);
    command
        .args(["refresh", "--all"])
        .args(args)
        .env("XDG_CONFIG_HOME", home.path())
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("SWAYSOCK")
        .output()
        .unwrap()
}

fn report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("bad report: {error}; output={output:?}"))
}

#[test]
fn no_instance_is_a_successful_non_activating_noop() {
    let bus = PrivateBus::start().unwrap();
    let output = refresh(&bus, &[]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        report(&output),
        serde_json::json!({"refreshed": 0, "failed": []})
    );
}

#[test]
fn normal_and_multiple_test_instances_are_isolated() {
    let bus = PrivateBus::start().unwrap();
    let normal = Peer::start(&bus, NORMAL, true, Duration::ZERO);
    let a = Peer::start(&bus, TEST_A, true, Duration::ZERO);
    let b = Peer::start(&bus, TEST_B, true, Duration::ZERO);
    let output = refresh(&bus, &[]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(report(&output)["refreshed"], 1);
    assert_eq!(normal.calls.load(Ordering::SeqCst), 1);
    assert_eq!(a.calls.load(Ordering::SeqCst), 0);
    assert_eq!(b.calls.load(Ordering::SeqCst), 0);
    let output = refresh(&bus, &["--test-instances"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(report(&output)["refreshed"], 2);
    assert_eq!(normal.calls.load(Ordering::SeqCst), 1);
    assert_eq!(a.calls.load(Ordering::SeqCst), 1);
    assert_eq!(b.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn older_peer_fails_clearly_without_blocking_supported_peer() {
    let bus = PrivateBus::start().unwrap();
    let _older = Peer::start(&bus, TEST_A, false, Duration::ZERO);
    let current = Peer::start(&bus, TEST_B, true, Duration::ZERO);
    let output = refresh(&bus, &["--test-instances"]);
    assert!(!output.status.success(), "{output:?}");
    let report = report(&output);
    assert_eq!(report["refreshed"], 1);
    assert_eq!(report["failed"][0]["instance"], TEST_A);
    assert!(
        report["failed"][0]["error"]
            .as_str()
            .unwrap()
            .contains("relaunch")
    );
    assert_eq!(current.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn unresponsive_peer_is_bounded_and_other_peers_are_still_dispatched() {
    let bus = PrivateBus::start().unwrap();
    let slow = Peer::start(&bus, TEST_A, true, Duration::from_secs(2));
    let fast = Peer::start(&bus, TEST_B, true, Duration::ZERO);
    let started = Instant::now();
    let output = refresh(&bus, &["--test-instances", "--timeout-seconds", "1"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(slow.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fast.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn missing_bus_and_autolaunch_are_errors_not_gui_launches() {
    for address in ["unix:path=/nonexistent/notm-test-bus", "autolaunch:"] {
        let output = Command::new(env!("CARGO_BIN_EXE_notm"))
            .args(["refresh", "--all", "--timeout-seconds", "1"])
            .env("DBUS_SESSION_BUS_ADDRESS", address)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .output()
            .unwrap();
        assert!(!output.status.success(), "{output:?}");
    }
}

#[test]
fn stalled_bus_authentication_is_covered_by_the_overall_deadline() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("bus");
    let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_notm"))
        .args(["refresh", "--all", "--timeout-seconds", "1"])
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}", path.display()),
        )
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(String::from_utf8_lossy(&output.stderr).contains("deadline"));
}

#[test]
fn standard_runtime_bus_fallback_needs_no_display_or_config() {
    let bus = PrivateBus::start().unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let path = bus
        .address
        .strip_prefix("unix:path=")
        .unwrap()
        .split(',')
        .next()
        .unwrap();
    std::os::unix::fs::symlink(path, runtime.path().join("bus")).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_notm"));
    command
        .args(["refresh", "--all"])
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env("XDG_RUNTIME_DIR", runtime.path());
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(report(&output)["refreshed"], 0);
}
