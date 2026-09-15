//! Where Jan's app-data root lives, and the one place that decision is made.
//!
//! `dirs::data_dir()`, `app.path().data_dir()` and `app.path().app_data_dir()`
//! all bottom out in the same per-OS lookup -- Tauri's resolvers are thin
//! wrappers over `dirs` (`tauri::path::desktop`). On Windows that lookup is
//! `SHGetKnownFolderPath(FOLDERID_RoamingAppData, 0, NULL, ..)`, which reads the
//! registry and ignores `%APPDATA%` entirely. With flags `0` and a null token,
//! the only input that can move the result is running as a different Windows
//! user.
//!
//! Every other desktop platform has a lever -- `HOME` on macOS, `HOME` plus
//! `XDG_DATA_HOME` on Linux -- and every other framework gives Windows one too:
//! Electron, Chromium and Python's `platformdirs` all read `%APPDATA%`. `dirs`
//! not reading it is a `dirs` design choice, not a Windows convention.
//!
//! So Jan resolves the root itself. On macOS and Linux these helpers are a
//! straight passthrough and resolve byte-identically to before. On Windows they
//! prefer `%APPDATA%` when it is set -- which Windows itself sets to the roaming
//! known folder for every interactive session, so real users land on the same
//! path as well. What changes is that the path becomes *addressable*: a test
//! harness, a portable install, or a support session can point a run at a
//! throwaway profile instead of the real one.

use std::ffi::OsString;
use std::path::PathBuf;

#[cfg(not(feature = "cli"))]
use tauri::{AppHandle, Manager, Runtime};

/// The rules for reading `%APPDATA%`, split out from the lookup so they are
/// testable on every platform rather than only where they take effect.
///
/// An empty value means "unset", not "the current directory" -- joining onto
/// `PathBuf::from("")` would silently produce a relative data folder. A
/// non-absolute value is rejected for the same reason: the point of the
/// override is to make the data root *addressable*, and a relative one would
/// instead follow the working directory, landing a different run in a different
/// folder. Windows itself always sets this to an absolute path, so rejecting
/// the rest costs nothing and falls back to the known folder.
///
/// `is_absolute()` uses the host's rules, which is what we want: the value only
/// ever takes effect on Windows, where it is read and judged by Windows.
fn appdata_override(raw: Option<OsString>) -> Option<PathBuf> {
    raw.map(PathBuf::from).filter(|value| value.is_absolute())
}

/// The Windows-only override, `None` everywhere else.
///
/// Gated with `cfg!` rather than `#[cfg]` so the body still type-checks on
/// macOS and Linux; the branch folds away at compile time, so the other
/// platforms get the same code they had before.
fn env_data_dir() -> Option<PathBuf> {
    if !cfg!(target_os = "windows") {
        return None;
    }
    appdata_override(std::env::var_os("APPDATA"))
}

/// The OS app-data root: `%APPDATA%` on Windows, otherwise `dirs::data_dir()`.
///
/// Use in place of `dirs::data_dir()`.
pub fn data_dir() -> Option<PathBuf> {
    env_data_dir().or_else(dirs::data_dir)
}

/// Same as [`data_dir`], for code that already holds an `AppHandle`.
///
/// Use in place of `app.path().data_dir()`. Falls through to Tauri's resolver
/// when there is no override, so the error type and the `UnknownPath` case are
/// unchanged.
#[cfg(not(feature = "cli"))]
pub fn data_dir_for<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<PathBuf> {
    match env_data_dir() {
        Some(dir) => Ok(dir),
        None => app.path().data_dir(),
    }
}

/// The bundle-identifier subdirectory of [`data_dir_for`], e.g.
/// `%APPDATA%/jan.ai.app`.
///
/// Use in place of `app.path().app_data_dir()`. Mirrors Tauri's own definition
/// (`data_dir()` joined with `config.identifier`) rather than the
/// `TAURI_BUNDLE_IDENTIFIER` constant, because
/// `.github/scripts/rename-tauri-app.sh` rewrites the identifier per release
/// channel (`jan-nightly.ai.app`, ...) and a nightly build has to keep
/// resolving to its own directory.
#[cfg(not(feature = "cli"))]
pub fn app_data_dir_for<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<PathBuf> {
    match env_data_dir() {
        Some(dir) => Ok(dir.join(&app.config().identifier)),
        None => app.path().app_data_dir(),
    }
}

/// The vector-db plugin's collection root, under [`data_dir`].
///
/// The plugin is a separate workspace crate and cannot call into this module,
/// so the app resolves the root and hands it over -- at `init()` for the Tauri
/// command surface, and per call for the agent memory store.
pub fn vector_db_dir() -> PathBuf {
    let root = data_dir().unwrap_or_else(|| PathBuf::from("."));
    tauri_plugin_vector_db::db::base_dir_in(&root)
}

#[cfg(test)]
mod tests {
    use super::*;

    // An absolute path spelled the way the host spells them, so the rule under
    // test is exercised on every platform rather than only on Windows.
    #[cfg(target_os = "windows")]
    const ABSOLUTE: &str = r"C:\throwaway\Roaming";
    #[cfg(not(target_os = "windows"))]
    const ABSOLUTE: &str = "/throwaway/Roaming";

    #[test]
    fn only_an_absolute_appdata_counts_as_set() {
        assert_eq!(appdata_override(None), None);
        assert_eq!(appdata_override(Some(OsString::from(""))), None);
        // Relative: would otherwise resolve against the working directory.
        assert_eq!(appdata_override(Some(OsString::from("Roaming"))), None);
        assert_eq!(
            appdata_override(Some(OsString::from(ABSOLUTE))),
            Some(PathBuf::from(ABSOLUTE))
        );
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn resolves_exactly_like_dirs_off_windows() {
        // No environment can move macOS or Linux off the path they resolved
        // before this module existed.
        assert!(env_data_dir().is_none());
        assert_eq!(data_dir(), dirs::data_dir());
    }

    // Runs on `test-on-windows-pr`, which is the only CI job that compiles this
    // block (rust-check.yml is ubuntu-only). Takes the process-wide env lock:
    // that job passes `--test-threads=1`, but a local `cargo test` does not.
    #[test]
    #[cfg(target_os = "windows")]
    fn appdata_outranks_the_known_folder() {
        let _guard = crate::core::server::provider_secrets::SECRET_STORE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let previous = std::env::var_os("APPDATA");

        std::env::set_var("APPDATA", r"C:\throwaway\Roaming");
        assert_eq!(data_dir(), Some(PathBuf::from(r"C:\throwaway\Roaming")));

        std::env::set_var("APPDATA", "");
        assert_eq!(data_dir(), dirs::data_dir());

        std::env::set_var("APPDATA", "Roaming");
        assert_eq!(data_dir(), dirs::data_dir());

        std::env::remove_var("APPDATA");
        assert_eq!(data_dir(), dirs::data_dir());

        match previous {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }
    }
}
