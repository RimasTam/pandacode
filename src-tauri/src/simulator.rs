//! iOS Simulator for the inspector and for agents. Xcode's `simctl` boots devices,
//! captures the screen and manages apps; AXe (an optional Homebrew CLI built on
//! Apple's simulator HID interface) supplies touches, typing, hardware buttons and
//! the accessibility tree. Every coordinate is in points, the unit taps use.
pub mod android;
pub mod android_setup;
pub mod commands;
pub mod stream;
pub mod tools;
mod tree;

use image::{codecs::jpeg::JpegEncoder, imageops::FilterType};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::Duration,
};

use crate::workspace::{checked, run_process, Capture};

const OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
const QUICK: Duration = Duration::from_secs(20);
const BOOT: Duration = Duration::from_secs(240);
const INSTALL: Duration = Duration::from_secs(180);
const MAX_TEXT: usize = 2000;
pub const AXE_INSTALL: &str = "brew install cameroncooke/axe/axe";

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub udid: String,
    pub name: String,
    pub runtime: String,
    pub state: String,
    /// `phone` or `pad`.
    pub family: String,
    pub device_type: String,
    /// `ios` or `android`.
    pub platform: String,
}
/// Screen geometry from the device type's profile.
#[derive(Debug, Serialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Screen {
    pub width: f64,
    pub height: f64,
    pub scale: f64,
    /// Devices without Face ID have a home button and a square-cornered display.
    pub home_button: bool,
}
#[derive(Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// Xcode's simulator tools answered, or (where iOS cannot run) the Android SDK is installed.
    pub available: bool,
    /// The iOS Simulator can run here (macOS only); elsewhere the panel is Android's alone.
    pub ios: bool,
    pub axe: bool,
    pub axe_install: String,
    /// The Android SDK's emulator is installed.
    pub android: bool,
    pub devices: Vec<Device>,
    pub selected: Option<String>,
    pub screen: Option<Screen>,
    pub error: Option<String>,
}
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Input {
    Tap { x: f64, y: f64 },
    #[serde(rename_all = "camelCase")]
    LongPress { x: f64, y: f64, duration_ms: Option<u64> },
    #[serde(rename_all = "camelCase")]
    Swipe { x: f64, y: f64, end_x: f64, end_y: f64, duration_ms: Option<u64> },
    Type { text: String },
    Button { button: String },
    /// A named key: return, backspace, tab, escape, space or an arrow.
    Key { key: String },
    TapLabel { label: String },
}

#[derive(Default)]
pub struct Simulators {
    selected: Mutex<Option<String>>,
    screens: Mutex<HashMap<String, Screen>>,
    pub streams: stream::Streams,
}

fn xcrun(args: &[&str], timeout: Duration) -> Result<Capture, String> {
    let mut command = Command::new("/usr/bin/xcrun");
    command.args(args);
    checked(run_process(command, "Simulator", OUTPUT_LIMIT, timeout).map_err(|error| {
        if error.starts_with("Could not start") { "Install Xcode to use the iOS Simulator.".to_string() } else { error }
    })?)
}

/// AXe from the user's PATH or a standard Homebrew prefix; GUI launches miss shell PATHs.
pub fn axe_path() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|path| std::env::split_paths(&path).collect()).unwrap_or_default();
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join(".local/bin"));
    }
    dirs.into_iter().map(|dir| dir.join("axe")).find(|path| path.is_file())
}
fn axe(args: &[&str], timeout: Duration) -> Result<Capture, String> {
    let path = axe_path().ok_or_else(|| format!("Touch input needs AXe. Install it with `{AXE_INSTALL}`."))?;
    let mut command = Command::new(path);
    command.args(args);
    checked(run_process(command, "AXe", OUTPUT_LIMIT, timeout)?)
}
fn json(capture: &Capture) -> Result<serde_json::Value, String> {
    serde_json::from_slice(&capture.bytes).map_err(|_| "The simulator tools returned an unexpected response.".into())
}

/// `com.apple.CoreSimulator.SimRuntime.iOS-26-2` → `iOS 26.2`.
fn runtime_name(key: &str) -> String {
    let tail = key.rsplit('.').next().unwrap_or(key);
    match tail.split_once('-') {
        Some((os, version)) => format!("{os} {}", version.replace('-', ".")),
        None => tail.to_string(),
    }
}
pub fn parse_devices(value: &serde_json::Value) -> Vec<Device> {
    let mut devices: Vec<Device> = value["devices"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(runtime, _)| runtime.contains("SimRuntime.iOS"))
        .flat_map(|(runtime, list)| {
            list.as_array().into_iter().flatten().filter(|d| d["isAvailable"].as_bool() != Some(false)).filter_map(move |d| {
                let device_type = d["deviceTypeIdentifier"].as_str().unwrap_or_default().to_string();
                Some(Device {
                    udid: d["udid"].as_str()?.to_string(),
                    name: d["name"].as_str()?.to_string(),
                    runtime: runtime_name(runtime),
                    state: d["state"].as_str().unwrap_or("Shutdown").to_string(),
                    family: if device_type.contains("iPad") { "pad" } else { "phone" }.into(),
                    device_type,
                    platform: "ios".into(),
                })
            })
        })
        .collect();
    // Booted first, then newest runtime, then name.
    devices.sort_by(|a, b| (b.state == "Booted").cmp(&(a.state == "Booted")).then(b.runtime.cmp(&a.runtime)).then(a.name.cmp(&b.name)));
    devices
}
/// Xcode, and with it the iOS Simulator, exists only on macOS.
pub const IOS: bool = cfg!(target_os = "macos");

/// iOS simulators and Android emulators; either platform alone is enough.
pub fn devices() -> Result<Vec<Device>, String> {
    let ios = IOS.then(|| xcrun(&["simctl", "list", "devices", "available", "-j"], QUICK).and_then(|output| json(&output)).map(|value| parse_devices(&value)));
    listing(ios, android::devices(), android::emulator_path().is_some())
}
/// `ios` is `None` where the iOS Simulator cannot run; there an installed SDK without AVDs
/// is an empty list, not an error.
fn listing(ios: Option<Result<Vec<Device>, String>>, android: Vec<Device>, sdk: bool) -> Result<Vec<Device>, String> {
    match ios {
        Some(Ok(mut list)) => { list.extend(android); Ok(sort(list)) }
        Some(Err(_)) if !android.is_empty() => Ok(sort(android)),
        Some(Err(error)) => Err(error),
        None if android.is_empty() && !sdk => Err(android::MISSING.into()),
        None => Ok(sort(android)),
    }
}
/// Running first, then iOS before Android, newest runtime, then name.
fn sort(mut devices: Vec<Device>) -> Vec<Device> {
    devices.sort_by(|a, b| (b.state == "Booted").cmp(&(a.state == "Booted")).then(a.platform.cmp(&b.platform).reverse()).then(b.runtime.cmp(&a.runtime)).then(a.name.cmp(&b.name)));
    devices
}

pub fn valid_udid(udid: &str) -> bool {
    match android::avd(udid) {
        Some(name) => android::valid_name(name),
        None => udid.len() == 36 && udid.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'),
    }
}

pub fn parse_screen(profile: &serde_json::Value) -> Option<Screen> {
    let scale = profile["mainScreenScale"].as_f64().filter(|s| *s > 0.0)?;
    Some(Screen {
        width: (profile["mainScreenWidth"].as_f64()? / scale).round(),
        height: (profile["mainScreenHeight"].as_f64()? / scale).round(),
        scale,
        home_button: profile["supportedFeatures"]["com.apple.pearl-id"].as_bool() != Some(true),
    })
}

impl Simulators {
    pub fn selected(&self) -> Option<String> {
        self.selected.lock().ok().and_then(|s| s.clone())
    }
    pub fn select(&self, udid: Option<String>) -> Result<(), String> {
        if udid.as_deref().is_some_and(|u| !valid_udid(u)) {
            return Err("Unknown simulator.".into());
        }
        *self.selected.lock().map_err(|e| e.to_string())? = udid;
        Ok(())
    }
    /// The device a request means: an explicit UDID, the one shown in the inspector, or the first booted one.
    pub fn resolve(&self, udid: Option<&str>, booted: bool) -> Result<Device, String> {
        let list = devices()?;
        let wanted = udid.map(str::to_string).or_else(|| self.selected());
        let device = match wanted {
            Some(udid) => list.into_iter().find(|d| d.udid == udid).ok_or("That simulator is not available.")?,
            None => list
                .into_iter()
                .find(|d| d.state == "Booted")
                .ok_or("No simulator is running. Boot one from the Simulator tab or with pandacode_simulator_manage.")?,
        };
        if booted && device.state != "Booted" {
            return Err(format!("{} is {}. Boot it first.", device.name, device.state.to_lowercase()));
        }
        Ok(device)
    }
    pub fn screen(&self, device: &Device) -> Result<Screen, String> {
        // Android screens are read from the running emulator, so they are cached per device.
        let key = if device.platform == "android" { device.udid.clone() } else { device.device_type.clone() };
        if let Some(screen) = self.screens.lock().map_err(|e| e.to_string())?.get(&key) {
            return Ok(*screen);
        }
        if let Some(name) = android::avd(&device.udid) {
            let screen = android::screen(name)?;
            self.screens.lock().map_err(|e| e.to_string())?.insert(key, screen);
            return Ok(screen);
        }
        let types = json(&xcrun(&["simctl", "list", "devicetypes", "-j"], QUICK)?)?;
        let bundle = types["devicetypes"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|t| t["identifier"] == device.device_type.as_str())
            .and_then(|t| t["bundlePath"].as_str())
            .ok_or("The device type of this simulator is unknown.")?
            .to_string();
        let mut command = Command::new("/usr/bin/plutil");
        command.args(["-convert", "json", "-o", "-"]).arg(Path::new(&bundle).join("Contents/Resources/profile.plist"));
        let profile = json(&checked(run_process(command, "plutil", OUTPUT_LIMIT, QUICK)?)?)?;
        let screen = parse_screen(&profile).ok_or("The device profile has no screen size.")?;
        self.screens.lock().map_err(|e| e.to_string())?.insert(device.device_type.clone(), screen);
        Ok(screen)
    }
    pub fn status(&self) -> Status {
        let mut status = Status { ios: IOS, axe: IOS && axe_path().is_some(), axe_install: AXE_INSTALL.into(), android: android::emulator_path().is_some(), selected: self.selected(), ..Default::default() };
        match devices() {
            Ok(list) => {
                status.available = true;
                if status.selected.as_ref().is_some_and(|u| !list.iter().any(|d| &d.udid == u)) {
                    status.selected = None;
                }
                let shown = status.selected.clone().and_then(|u| list.iter().find(|d| d.udid == u).cloned());
                status.screen = shown.filter(|d| d.platform == "ios" || d.state == "Booted").and_then(|d| self.screen(&d).ok());
                status.devices = list;
            }
            Err(error) => status.error = Some(error),
        }
        status
    }
    /// Forget a device's cached screen, e.g. after it boots or shuts down.
    pub fn forget_screen(&self, udid: &str) {
        if let Ok(mut screens) = self.screens.lock() { screens.remove(udid); }
    }
}

pub fn boot(udid: &str) -> Result<(), String> {
    if let Some(name) = android::avd(udid) { return android::boot(name); }
    if let Err(error) = xcrun(&["simctl", "boot", udid], BOOT) {
        if !error.contains("current state: Booted") {
            return Err(error);
        }
    }
    xcrun(&["simctl", "bootstatus", udid, "-b"], BOOT).map(|_| ())
}
pub fn shutdown(udid: &str) -> Result<(), String> {
    if let Some(name) = android::avd(udid) { return android::shutdown(name); }
    match xcrun(&["simctl", "shutdown", udid], QUICK) {
        Err(error) if !error.contains("current state: Shutdown") => Err(error),
        _ => Ok(()),
    }
}

/// A PNG of the screen at full resolution, or JPEG resized to points when `points` is set.
pub fn screenshot(udid: &str, points: Option<Screen>) -> Result<Vec<u8>, String> {
    let dir = std::env::temp_dir().join("pandacode-simulator");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stem = dir.join(format!("{}-{}", udid, crate::storage::id()));
    let (raw, out) = (stem.with_extension("png"), stem.with_extension("jpg"));
    let result = (|| {
        match android::avd(udid) {
            Some(name) => {
                let png = android::screenshot(name)?;
                return match points { Some(screen) => jpeg_in_points(&png, screen), None => Ok(png) };
            }
            None => { xcrun(&["simctl", "io", udid, "screenshot", "--type=png", &raw.to_string_lossy()], QUICK)?; }
        }
        let Some(screen) = points else { return std::fs::read(&raw).map_err(|e| e.to_string()) };
        let mut command = Command::new("/usr/bin/sips");
        command
            .args(["-s", "format", "jpeg", "-s", "formatOptions", "80", "--resampleHeightWidth"])
            .arg(screen.height.to_string())
            .arg(screen.width.to_string())
            .arg(&raw)
            .arg("--out")
            .arg(&out);
        checked(run_process(command, "sips", 64 * 1024, QUICK)?)?;
        std::fs::read(&out).map_err(|e| e.to_string())
    })();
    let _ = std::fs::remove_file(&raw);
    let _ = std::fs::remove_file(&out);
    result
}

/// An Android screenshot as a JPEG whose pixels are points, resized here because `sips`
/// exists only on macOS.
fn jpeg_in_points(png: &[u8], screen: Screen) -> Result<Vec<u8>, String> {
    let size = |points: f64| points.round().max(1.0) as u32;
    let image = image::load_from_memory(png).map_err(|e| format!("The emulator screenshot could not be read: {e}"))?;
    let image = image.resize_exact(size(screen.width), size(screen.height), FilterType::Triangle);
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 80).encode_image(&image.to_rgb8()).map_err(|e| e.to_string())?;
    Ok(jpeg)
}

fn point(value: f64, limit: f64, axis: &str) -> Result<String, String> {
    if !value.is_finite() || value < 0.0 || value > limit {
        return Err(format!("{axis} must be within the screen (0–{limit} points)."));
    }
    Ok(format!("{value:.1}"))
}
const BUTTONS: [&str; 5] = ["home", "lock", "side-button", "siri", "apple-pay"];
/// HID usage IDs for the keys an agent or the panel can press.
pub const KEYS: [(&str, u8); 10] = [("return", 40), ("escape", 41), ("backspace", 42), ("tab", 43), ("space", 44), ("right", 79), ("left", 80), ("down", 81), ("up", 82), ("delete", 76)];

/// Deliver one input through AXe. Coordinates are points within `screen`.
pub fn input(udid: &str, screen: Screen, input: &Input) -> Result<String, String> {
    if let Some(name) = android::avd(udid) { return android::input(name, screen, input); }
    let (w, h) = (screen.width, screen.height);
    match input {
        Input::Tap { x, y } => {
            axe(&["tap", "-x", &point(*x, w, "x")?, "-y", &point(*y, h, "y")?, "--udid", udid], QUICK)?;
            Ok(format!("Tapped ({x:.0}, {y:.0})."))
        }
        Input::LongPress { x, y, duration_ms } => {
            let hold = format!("{:.2}", duration_ms.unwrap_or(800).clamp(100, 10_000) as f64 / 1000.0);
            axe(&["touch", "-x", &point(*x, w, "x")?, "-y", &point(*y, h, "y")?, "--down", "--up", "--delay", &hold, "--udid", udid], QUICK)?;
            Ok(format!("Long-pressed ({x:.0}, {y:.0}) for {hold}s."))
        }
        Input::Swipe { x, y, end_x, end_y, duration_ms } => {
            let duration = format!("{:.2}", duration_ms.unwrap_or(300).clamp(50, 5_000) as f64 / 1000.0);
            axe(&["swipe", "--start-x", &point(*x, w, "x")?, "--start-y", &point(*y, h, "y")?, "--end-x", &point(*end_x, w, "endX")?, "--end-y", &point(*end_y, h, "endY")?, "--duration", &duration, "--udid", udid], QUICK)?;
            Ok(format!("Swiped from ({x:.0}, {y:.0}) to ({end_x:.0}, {end_y:.0})."))
        }
        Input::Type { text } => {
            if text.is_empty() || text.chars().count() > MAX_TEXT || text.contains('\0') {
                return Err(format!("Type between 1 and {MAX_TEXT} characters."));
            }
            // `--` keeps text such as `-v` from being read as an option.
            axe(&["type", "--udid", udid, "--", text], Duration::from_secs(60))?;
            Ok(format!("Typed {} characters.", text.chars().count()))
        }
        Input::Button { button } => {
            if !BUTTONS.contains(&button.as_str()) {
                return Err(format!("Press one of: {}.", BUTTONS.join(", ")));
            }
            axe(&["button", button, "--udid", udid], QUICK)?;
            Ok(format!("Pressed {button}."))
        }
        Input::Key { key } => {
            let code = KEYS.iter().find(|(name, _)| name == key).map(|(_, code)| code.to_string()).ok_or_else(|| format!("Press one of: {}.", KEYS.map(|(name, _)| name).join(", ")))?;
            axe(&["key", &code, "--udid", udid], QUICK)?;
            Ok(format!("Pressed {key}."))
        }
        Input::TapLabel { label } => {
            if label.trim().is_empty() || label.len() > 500 {
                return Err("Give the accessibility label to tap.".into());
            }
            let output = axe(&["tap", "--label", label, "--wait-timeout", "2", "--udid", udid], QUICK)?;
            let report = String::from_utf8_lossy(&output.bytes).trim().to_string();
            Ok(if report.is_empty() { format!("Tapped \"{label}\".") } else { report })
        }
    }
}

/// The accessibility tree as compact lines, or the element at one point.
pub fn describe(udid: &str, at: Option<(f64, f64)>) -> Result<String, String> {
    if let Some(name) = android::avd(udid) { return android::describe(name, android::screen(name)?.scale); }
    let at = at.map(|(x, y)| format!("{x:.1},{y:.1}"));
    let mut args = vec!["describe-ui", "--udid", udid];
    if let Some(at) = at.as_deref() {
        args.extend(["--point", at]);
    }
    Ok(tree::summarize(&json(&axe(&args, Duration::from_secs(45))?)?))
}

fn valid_bundle_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 255 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') || id.starts_with('-') {
        return Err("Give a bundle identifier such as com.example.App.".into());
    }
    Ok(())
}
pub fn launch(udid: &str, bundle_id: &str) -> Result<String, String> {
    if let Some(name) = android::avd(udid) { return android::launch(name, bundle_id); }
    valid_bundle_id(bundle_id)?;
    xcrun(&["simctl", "launch", "--terminate-running-process", udid, bundle_id], QUICK)?;
    Ok(format!("Launched {bundle_id}."))
}
pub fn terminate(udid: &str, bundle_id: &str) -> Result<String, String> {
    if let Some(name) = android::avd(udid) { return android::terminate(name, bundle_id); }
    valid_bundle_id(bundle_id)?;
    xcrun(&["simctl", "terminate", udid, bundle_id], QUICK)?;
    Ok(format!("Terminated {bundle_id}."))
}
pub fn open_url(udid: &str, url: &str) -> Result<String, String> {
    if let Some(name) = android::avd(udid) { return android::open_url(name, url); }
    let scheme_ok = url.split_once(':').is_some_and(|(scheme, _)| !scheme.is_empty() && scheme.bytes().all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b)));
    if url.len() > 4096 || url.chars().any(char::is_whitespace) || !scheme_ok {
        return Err("Give a URL such as https://example.com or myapp://path.".into());
    }
    xcrun(&["simctl", "openurl", udid, url], QUICK)?;
    Ok(format!("Opened {url}."))
}
/// Install a built `.app` bundle, e.g. from Xcode's DerivedData.
pub fn install(udid: &str, path: &Path) -> Result<String, String> {
    if let Some(name) = android::avd(udid) { return android::install(name, path); }
    let path = path.canonicalize().map_err(|_| "That app bundle does not exist.".to_string())?;
    if path.extension().and_then(|e| e.to_str()) != Some("app") || !path.is_dir() {
        return Err("Give the path of a built .app bundle for the simulator.".into());
    }
    xcrun(&["simctl", "install", udid, &path.to_string_lossy()], INSTALL)?;
    Ok(format!("Installed {}.", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()))
}
pub fn appearance(udid: &str, mode: &str) -> Result<String, String> {
    if let Some(name) = android::avd(udid) { return android::appearance(name, mode); }
    if !["light", "dark"].contains(&mode) {
        return Err("Appearance is light or dark.".into());
    }
    xcrun(&["simctl", "ui", udid, "appearance", mode], QUICK)?;
    Ok(format!("Appearance set to {mode}."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_list_ios_simulators_booted_first() {
        let value = serde_json::json!({"devices":{
            "com.apple.CoreSimulator.SimRuntime.iOS-26-2":[
                {"udid":"3720D4A0-B8C5-4DAC-9425-6A4685BC533E","name":"iPhone 17 Pro","state":"Shutdown","isAvailable":true,"deviceTypeIdentifier":"com.apple.CoreSimulator.SimDeviceType.iPhone-17-Pro"},
                {"udid":"73BD9B70-5E3A-49BD-B2C9-AA7256897283","name":"iPad Air","state":"Booted","isAvailable":true,"deviceTypeIdentifier":"com.apple.CoreSimulator.SimDeviceType.iPad-Air-11-inch-M3"},
                {"udid":"FD7CC5DD-38EC-4008-A6BA-D62FF4BC3728","name":"Broken","state":"Shutdown","isAvailable":false,"deviceTypeIdentifier":"x"}],
            "com.apple.CoreSimulator.SimRuntime.watchOS-26-0":[{"udid":"AAAAAAAA-B8C5-4DAC-9425-6A4685BC533E","name":"Watch","state":"Booted","isAvailable":true,"deviceTypeIdentifier":"w"}]}});
        let list = parse_devices(&value);
        assert_eq!(list.iter().map(|d| (d.name.as_str(), d.family.as_str(), d.runtime.as_str())).collect::<Vec<_>>(), vec![("iPad Air", "pad", "iOS 26.2"), ("iPhone 17 Pro", "phone", "iOS 26.2")]);
    }

    #[test]
    fn android_alone_fills_the_list_where_ios_cannot_run() {
        let pixel = || Device { udid: "android:Pixel_9_API_35".into(), name: "Pixel 9 API 35".into(), runtime: "Android 15".into(), state: "Shutdown".into(), family: "phone".into(), device_type: "android:pixel_9".into(), platform: "android".into() };
        // macOS without Xcode still lists Android emulators; with neither, Xcode's error stands.
        assert_eq!(listing(Some(Err("Install Xcode".into())), vec![pixel()], true), Ok(vec![pixel()]));
        assert_eq!(listing(Some(Err("Install Xcode".into())), vec![], false), Err("Install Xcode".into()));
        // Windows: never Xcode's error, and an SDK without AVDs is an empty list.
        assert_eq!(listing(None, vec![pixel()], true), Ok(vec![pixel()]));
        assert_eq!(listing(None, vec![], true), Ok(vec![]));
        assert_eq!(listing(None, vec![], false), Err(android::MISSING.into()));
    }

    #[test]
    fn android_screenshots_shrink_to_points_without_sips() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(1080, 2400, image::Rgba([20, 120, 220, 255])).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let jpeg = jpeg_in_points(&png, Screen { width: 411.4, height: 914.3, scale: 2.625, home_button: false }).unwrap();
        let shrunk = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((shrunk.width(), shrunk.height()), (411, 914));
        assert!(jpeg.starts_with(&[0xff, 0xd8]), "a JPEG, as the agent tools label it");
        assert!(jpeg_in_points(b"not a png", Screen { width: 1.0, height: 1.0, scale: 1.0, home_button: false }).is_err());
    }

    #[test]
    fn screens_come_from_device_profiles() {
        let profile = serde_json::json!({"mainScreenScale":3,"mainScreenWidth":1206,"mainScreenHeight":2622,"supportedFeatures":{"com.apple.pearl-id":true}});
        assert_eq!(parse_screen(&profile), Some(Screen { width: 402.0, height: 874.0, scale: 3.0, home_button: false }));
        let se = serde_json::json!({"mainScreenScale":2,"mainScreenWidth":750,"mainScreenHeight":1334,"supportedFeatures":{}});
        assert!(parse_screen(&se).unwrap().home_button);
        assert!(parse_screen(&serde_json::json!({"mainScreenScale":0})).is_none());
    }

    #[test]
    fn inputs_are_bounded_before_any_process_runs() {
        assert!(point(10.0, 402.0, "x").is_ok());
        assert!(point(403.0, 402.0, "x").is_err());
        assert!(point(f64::NAN, 402.0, "x").is_err());
        assert!(valid_bundle_id("com.example.App").is_ok());
        assert!(valid_bundle_id("-rf").is_err());
        assert!(valid_bundle_id("com.example App").is_err());
        assert!(valid_udid("3720D4A0-B8C5-4DAC-9425-6A4685BC533E"));
        assert!(!valid_udid("booted"));
        assert_eq!(runtime_name("com.apple.CoreSimulator.SimRuntime.iOS-26-2"), "iOS 26.2");
        let input: Input = serde_json::from_value(serde_json::json!({"kind":"swipe","x":1,"y":2,"endX":3,"endY":4})).unwrap();
        assert_eq!(input, Input::Swipe { x: 1.0, y: 2.0, end_x: 3.0, end_y: 4.0, duration_ms: None });
    }

    /// Drives a booted simulator end to end; run with `cargo test -- --ignored simulator::tests::live` with one booted and AXe installed.
    #[test]
    #[ignore]
    fn live_booted_simulator_answers_every_call() {
        let sims = Simulators::default();
        let device = sims.resolve(None, true).expect("boot a simulator first");
        let screen = sims.screen(&device).unwrap();
        assert!(screen.width > 300.0 && screen.scale >= 2.0, "{screen:?}");
        let jpeg = screenshot(&device.udid, Some(screen)).unwrap();
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
        input(&device.udid, screen, &Input::Button { button: "home".into() }).unwrap();
        let tree = describe(&device.udid, None).unwrap();
        assert!(tree.contains(&format!("Screen {}×{}", screen.width, screen.height)), "{tree}");
        input(&device.udid, screen, &Input::Swipe { x: 300.0, y: 500.0, end_x: 60.0, end_y: 500.0, duration_ms: None }).unwrap();
        input(&device.udid, screen, &Input::Button { button: "home".into() }).unwrap();
        let streams = stream::Streams::default();
        let frame = streams.next(&device.udid, 0, Duration::from_secs(10)).unwrap().expect("a live frame");
        assert_eq!(&frame.jpeg[..2], &[0xFF, 0xD8]);
        let later = streams.next(&device.udid, frame.seq, Duration::from_secs(5)).unwrap().expect("frames keep coming");
        assert!(later.seq > frame.seq);
        streams.stop(&device.udid);
    }

}
