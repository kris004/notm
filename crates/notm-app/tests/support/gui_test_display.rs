use std::{
    env,
    ffi::OsStr,
    fs::{self, OpenOptions},
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Condvar, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, ensure};

const GUI_TEST_DISPLAY_ENV: &str = "NOTM_GUI_TEST_DISPLAY";
const REQUIRE_GTK_DISPLAY_ENV: &str = "NOTM_REQUIRE_GTK_DISPLAY";
const OFFSCREEN_COMPOSITOR: &str = "sway";
const OFFSCREEN_STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const OFFSCREEN_STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(25);
const MAX_PARALLEL_GUI_TESTS: usize = 2;

pub(crate) struct GuiTestDisplay {
    target: DisplayTarget,
    _permit: GuiTestPermit,
}

enum DisplayTarget {
    Provided,
    Offscreen(HeadlessSway),
}

impl GuiTestDisplay {
    pub(crate) fn start(work_dir: &Path) -> anyhow::Result<Self> {
        let permit = GuiTestPermit::acquire();
        let mode = requested_display_mode(|name| env::var(name).ok())?;
        let target = match mode {
            DisplayMode::Provided => {
                ensure!(
                    provided_display_environment(|name| env::var(name).ok()).is_some(),
                    "{GUI_TEST_DISPLAY_ENV}=provided requires a non-empty DISPLAY or \
                     WAYLAND_DISPLAY"
                );
                DisplayTarget::Provided
            }
            DisplayMode::Offscreen => {
                ensure!(
                    command_is_available(OFFSCREEN_COMPOSITOR),
                    "offscreen GUI tests require `{OFFSCREEN_COMPOSITOR}` in PATH"
                );
                DisplayTarget::Offscreen(HeadlessSway::start(work_dir)?)
            }
        };
        Ok(Self {
            target,
            _permit: permit,
        })
    }

    pub(crate) fn configure_command(&self, command: &mut Command) {
        if let DisplayTarget::Offscreen(display) = &self.target {
            display.configure_command(command);
        }
    }

    pub(crate) fn diagnostic_log(&self) -> Option<String> {
        match &self.target {
            DisplayTarget::Provided => None,
            DisplayTarget::Offscreen(display) => Some(display.logs()),
        }
    }
}

pub(crate) fn gtk_display_environment() -> anyhow::Result<Option<String>> {
    let required = env::var(REQUIRE_GTK_DISPLAY_ENV).is_ok_and(|value| value == "1");
    resolve_display_environment(
        |name| env::var(name).ok(),
        command_is_available(OFFSCREEN_COMPOSITOR),
        required,
    )
}

fn resolve_display_environment(
    mut get_variable: impl FnMut(&str) -> Option<String>,
    offscreen_compositor_available: bool,
    required: bool,
) -> anyhow::Result<Option<String>> {
    let mode = requested_display_mode(&mut get_variable)?;
    let display = match mode {
        DisplayMode::Offscreen if offscreen_compositor_available => Some(format!(
            "offscreen {OFFSCREEN_COMPOSITOR} (headless Wayland)"
        )),
        DisplayMode::Offscreen => None,
        DisplayMode::Provided => provided_display_environment(get_variable),
    };
    ensure!(
        !required || display.is_some(),
        "{REQUIRE_GTK_DISPLAY_ENV}=1 requires an available GUI test display"
    );
    Ok(display)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DisplayMode {
    Offscreen,
    Provided,
}

fn requested_display_mode(
    mut get_variable: impl FnMut(&str) -> Option<String>,
) -> anyhow::Result<DisplayMode> {
    match get_variable(GUI_TEST_DISPLAY_ENV).as_deref() {
        None | Some("") | Some("offscreen") => Ok(DisplayMode::Offscreen),
        Some("provided" | "live") => Ok(DisplayMode::Provided),
        Some(value) => anyhow::bail!(
            "unsupported {GUI_TEST_DISPLAY_ENV} value {value:?}; use `offscreen` or `provided`"
        ),
    }
}

fn provided_display_environment(
    mut get_variable: impl FnMut(&str) -> Option<String>,
) -> Option<String> {
    ["WAYLAND_DISPLAY", "DISPLAY"].into_iter().find_map(|name| {
        get_variable(name)
            .filter(|value| !value.is_empty())
            .map(|value| format!("{name}={value}"))
    })
}

fn command_is_available(command: &str) -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path).any(|directory| {
        let candidate = directory.join(command);
        candidate.is_file()
            && candidate
                .metadata()
                .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
    })
}

struct HeadlessSway {
    child: Child,
    runtime_dir: PathBuf,
    wayland_display: String,
    log_path: PathBuf,
}

impl HeadlessSway {
    fn start(work_dir: &Path) -> anyhow::Result<Self> {
        let display_dir = work_dir.join("gui-display");
        let runtime_dir = display_dir.join("runtime");
        let config_home = display_dir.join("config");
        let cache_home = display_dir.join("cache");
        let data_home = display_dir.join("data");
        let home = display_dir.join("home");
        for directory in [
            &display_dir,
            &runtime_dir,
            &config_home,
            &cache_home,
            &data_home,
            &home,
        ] {
            fs::create_dir_all(directory)
                .with_context(|| format!("creating GUI test directory {}", directory.display()))?;
        }
        fs::set_permissions(&runtime_dir, fs::Permissions::from_mode(0o700))?;

        let config_path = display_dir.join("sway.conf");
        fs::write(
            &config_path,
            "xwayland disable\n\
             focus_follows_mouse no\n\
             default_border pixel 1\n\
             output * mode 1920x1080\n\
             output * background #202020 solid_color\n",
        )?;
        let log_path = display_dir.join("sway.log");
        let log = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&log_path)
            .with_context(|| format!("creating compositor log {}", log_path.display()))?;

        let mut command = Command::new(OFFSCREEN_COMPOSITOR);
        command
            .args([OsStr::new("--config"), config_path.as_os_str()])
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("SWAYSOCK")
            .env("HOME", &home)
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_CACHE_HOME", &cache_home)
            .env("XDG_DATA_HOME", &data_home)
            .env("WLR_BACKENDS", "headless")
            .env("WLR_HEADLESS_OUTPUTS", "1")
            .env("WLR_RENDERER", "pixman")
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        let mut child = command.spawn().context("launching headless Sway")?;

        let deadline = Instant::now() + OFFSCREEN_STARTUP_TIMEOUT;
        loop {
            if let Some(status) = child.try_wait()? {
                anyhow::bail!(
                    "headless Sway exited during startup with {status}\n{}",
                    fs::read_to_string(&log_path).unwrap_or_default()
                );
            }
            if let Some(wayland_display) = wayland_socket_name(&runtime_dir)? {
                return Ok(Self {
                    child,
                    runtime_dir,
                    wayland_display,
                    log_path,
                });
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!(
                    "headless Sway did not expose a Wayland socket within \
                     {OFFSCREEN_STARTUP_TIMEOUT:?}\n{}",
                    fs::read_to_string(&log_path).unwrap_or_default()
                );
            }
            thread::sleep(OFFSCREEN_STARTUP_POLL_INTERVAL);
        }
    }

    fn configure_command(&self, command: &mut Command) {
        command
            .env_remove("DISPLAY")
            .env_remove("SWAYSOCK")
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .env("WAYLAND_DISPLAY", &self.wayland_display)
            .env("GDK_BACKEND", "wayland");
    }

    fn logs(&self) -> String {
        fs::read_to_string(&self.log_path)
            .unwrap_or_else(|err| format!("could not read compositor log: {err}"))
    }
}

impl Drop for HeadlessSway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wayland_socket_name(runtime_dir: &Path) -> anyhow::Result<Option<String>> {
    for entry in fs::read_dir(runtime_dir)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with("wayland-")
            && entry.file_type()?.is_socket()
        {
            return Ok(Some(entry.file_name().to_string_lossy().into_owned()));
        }
    }
    Ok(None)
}

struct GuiTestSlots {
    active: Mutex<usize>,
    wake: Condvar,
}

impl GuiTestSlots {
    fn shared() -> &'static Self {
        static SLOTS: OnceLock<GuiTestSlots> = OnceLock::new();
        SLOTS.get_or_init(|| GuiTestSlots {
            active: Mutex::new(0),
            wake: Condvar::new(),
        })
    }
}

struct GuiTestPermit;

impl GuiTestPermit {
    fn acquire() -> Self {
        let slots = GuiTestSlots::shared();
        let mut active = slots
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *active >= MAX_PARALLEL_GUI_TESTS {
            active = slots
                .wake
                .wait(active)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *active += 1;
        Self
    }
}

impl Drop for GuiTestPermit {
    fn drop(&mut self) {
        let slots = GuiTestSlots::shared();
        let mut active = slots
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *active = active.saturating_sub(1);
        slots.wake.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offscreen_is_default_even_when_a_provided_display_exists() {
        let display = resolve_display_environment(
            |name| match name {
                "WAYLAND_DISPLAY" => Some("wayland-user".to_string()),
                _ => None,
            },
            true,
            true,
        )
        .expect("default display selection");

        assert_eq!(
            display.as_deref(),
            Some("offscreen sway (headless Wayland)")
        );
    }

    #[test]
    fn provided_display_requires_an_explicit_mode_and_nonempty_name() {
        let display = resolve_display_environment(
            |name| match name {
                GUI_TEST_DISPLAY_ENV => Some("provided".to_string()),
                "WAYLAND_DISPLAY" => Some(String::new()),
                "DISPLAY" => Some(":42".to_string()),
                _ => None,
            },
            true,
            true,
        )
        .expect("explicit provided display selection");

        assert_eq!(display.as_deref(), Some("DISPLAY=:42"));
    }

    #[test]
    fn live_remains_an_alias_for_a_provided_display() {
        let display = resolve_display_environment(
            |name| match name {
                GUI_TEST_DISPLAY_ENV => Some("live".to_string()),
                "WAYLAND_DISPLAY" => Some("wayland-ci".to_string()),
                _ => None,
            },
            false,
            true,
        )
        .expect("legacy live display selection");

        assert_eq!(display.as_deref(), Some("WAYLAND_DISPLAY=wayland-ci"));
    }

    #[test]
    fn required_mode_rejects_an_unavailable_offscreen_compositor() {
        let error = resolve_display_environment(|_| None, false, true)
            .expect_err("required-display mode must reject a missing compositor");

        assert!(
            error.to_string().contains(REQUIRE_GTK_DISPLAY_ENV),
            "required-display error did not name the opt-in: {error}"
        );
        assert_eq!(
            resolve_display_environment(|_| None, false, false).expect("optional display gate"),
            None
        );
    }

    #[test]
    fn invalid_display_mode_is_rejected() {
        let error = resolve_display_environment(
            |name| (name == GUI_TEST_DISPLAY_ENV).then(|| "nested".to_string()),
            true,
            false,
        )
        .expect_err("unsupported display mode must fail");

        assert!(
            error.to_string().contains("offscreen` or `provided"),
            "{error}"
        );
    }
}
