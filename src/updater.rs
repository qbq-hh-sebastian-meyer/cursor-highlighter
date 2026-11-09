use reqwest::{Url, blocking::Client, redirect::Policy};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::{self, File},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, GetLastError, HANDLE, WAIT_OBJECT_0},
    System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
};

const REPOSITORY: &str = "qbq-hh-sebastian-meyer/cursor-highlighter";
const RELEASES: &str =
    "https://api.github.com/repos/qbq-hh-sebastian-meyer/cursor-highlighter/releases?per_page=100";
const PREFIX: &str = ".cursor-highlighter-update-";
const MAX_ARCHIVE: u64 = 32 * 1024 * 1024;
const MAX_EXE: u64 = 64 * 1024 * 1024;
const PERIOD: Duration = Duration::from_secs(24 * 60 * 60);
const MANUAL_COOLDOWN: Duration = Duration::from_secs(60);

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn architecture() -> Result<(&'static str, u16), String> {
    match std::env::consts::ARCH {
        "aarch64" => Ok(("arm64", 0xAA64)),
        "x86_64" => Ok(("x64", 0x8664)),
        _ => Err("Automatic updates support only Windows ARM64 and x64.".into()),
    }
}

#[derive(Clone, Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Debug)]
struct Package {
    version: Version,
    archive: Asset,
    checksum: Asset,
}

fn asset_url(asset: &Asset, tag: &str) -> Result<(), String> {
    let url = Url::parse(&asset.browser_download_url).map_err(err)?;
    let expected = format!("/{REPOSITORY}/releases/download/{tag}/{}", asset.name);
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != expected
    {
        return Err("The release contains an unexpected asset URL.".into());
    }
    Ok(())
}

fn select(
    releases: Vec<Release>,
    current: &Version,
    arch: &str,
) -> Result<Option<Package>, String> {
    let latest = releases
        .into_iter()
        .filter(|release| !release.draft && !release.prerelease)
        .filter_map(|release| {
            Version::parse(
                release
                    .tag_name
                    .strip_prefix('v')
                    .unwrap_or(&release.tag_name),
            )
            .ok()
            .filter(|version| version.pre.is_empty() && version.cmp_precedence(current).is_gt())
            .map(|version| (version, release))
        })
        .max_by(|(left, _), (right, _)| left.cmp_precedence(right));
    let Some((version, release)) = latest else {
        return Ok(None);
    };
    let name = format!("cursor-highlighter-windows-{arch}.zip");
    let find = |name: &str, limit: u64| -> Result<Asset, String> {
        let matches: Vec<_> = release.assets.iter().filter(|a| a.name == name).collect();
        if matches.len() != 1 {
            return Err(format!("Release {version} needs exactly one {name} asset."));
        }
        let asset = matches[0];
        if asset.size == 0 || asset.size > limit {
            return Err(format!("Release asset {name} exceeds the allowed size."));
        }
        asset_url(asset, &release.tag_name)?;
        Ok(asset.clone())
    };
    Ok(Some(Package {
        version: version.clone(),
        archive: find(&name, MAX_ARCHIVE)?,
        checksum: find(&format!("{name}.sha256"), 1024)?,
    }))
}

fn trusted_redirect(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(
            url.host_str(),
            Some(
                "github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
            )
        )
}

fn client() -> Result<Client, String> {
    Client::builder()
        .user_agent(concat!("CursorHighlighter/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .https_only(true)
        .redirect(Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 || !trusted_redirect(attempt.url()) {
                attempt.error("Refused an untrusted release redirect")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|_| "Could not initialize the HTTPS updater.".into())
}

fn bounded_read(reader: impl Read, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(err)?;
    if bytes.len() as u64 > limit {
        return Err("The update response exceeds the allowed size.".into());
    }
    Ok(bytes)
}

fn download(client: &Client, url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let response = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|_| "Update connection failed or timed out. Check your network and try again.")?;
    if !response.status().is_success() {
        return Err(format!(
            "Update server returned HTTP {}. Try again later (GitHub may be rate limiting).",
            response.status().as_u16()
        ));
    }
    if response.content_length().is_some_and(|size| size > limit) {
        return Err("The update response exceeds the allowed size.".into());
    }
    bounded_read(response, limit)
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn checksum(bytes: &[u8], name: &str) -> Result<String, String> {
    let text = std::str::from_utf8(bytes).map_err(err)?;
    let parts: Vec<_> = text.split_whitespace().collect();
    if parts.len() != 2
        || parts[0].len() != 64
        || !parts[0].bytes().all(|b| b.is_ascii_hexdigit())
        || parts[1] != name
    {
        return Err("The release SHA256 file is invalid or names a different archive.".into());
    }
    Ok(parts[0].to_ascii_lowercase())
}

fn executable(archive: &[u8], machine: u16) -> Result<Vec<u8>, String> {
    let mut zip = zip::ZipArchive::new(Cursor::new(archive)).map_err(err)?;
    if zip.is_empty() || zip.len() > 2 {
        return Err(
            "The update ZIP must contain only the executable and optional README.md.".into(),
        );
    }
    let mut exe = None;
    let mut readme = false;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(err)?;
        let mode = entry.unix_mode().unwrap_or(0) & 0o170000;
        if entry.is_dir() || (mode != 0 && mode != 0o100000) {
            return Err("The update ZIP contains a non-regular file.".into());
        }
        match entry.name() {
            "cursor-highlighter.exe" if exe.is_none() && entry.size() <= MAX_EXE => {
                exe = Some(bounded_read(&mut entry, MAX_EXE)?);
            }
            "README.md" if !readme && entry.size() <= 1024 * 1024 => {
                readme = true;
                bounded_read(&mut entry, 1024 * 1024)?;
            }
            _ => {
                return Err(
                    "The update ZIP contains an unexpected, duplicate or oversized file.".into(),
                );
            }
        }
    }
    let exe = exe.ok_or("The update ZIP has no cursor-highlighter.exe.")?;
    validate_pe(&exe, machine)?;
    Ok(exe)
}

fn validate_pe(bytes: &[u8], machine: u16) -> Result<(), String> {
    let valid = (|| {
        if bytes.get(..2)? != b"MZ" {
            return None;
        }
        let offset = u32::from_le_bytes(bytes.get(0x3c..0x40)?.try_into().ok()?) as usize;
        let header = bytes.get(offset..offset.checked_add(26)?)?;
        let optional = u16::from_le_bytes(header[24..26].try_into().ok()?);
        let actual = u16::from_le_bytes(header[4..6].try_into().ok()?);
        let characteristics = u16::from_le_bytes(header[22..24].try_into().ok()?);
        (header[..4] == *b"PE\0\0"
            && actual == machine
            && optional == 0x20b
            && characteristics & 0x0002 != 0
            && characteristics & 0x2000 == 0)
            .then_some(())
    })();
    valid.ok_or("The update executable is invalid or has the wrong architecture.".into())
}

fn regular(path: &Path) -> Result<(), String> {
    use std::os::windows::fs::MetadataExt;
    let metadata = fs::symlink_metadata(path).map_err(err)?;
    if !metadata.is_file() || metadata.file_attributes() & 0x400 != 0 {
        return Err(format!(
            "Not a regular, non-reparse file: {}",
            path.display()
        ));
    }
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(err)?;
    file.write_all(bytes).map_err(err)?;
    file.sync_all().map_err(err)
}

#[derive(Debug)]
pub struct Staged {
    directory: TempDir,
    target: PathBuf,
    pub version: Version,
    sha256: String,
}

fn check_and_stage(
    target: PathBuf,
    known_version: Option<Version>,
    cancelled: &AtomicBool,
    status: impl Fn(&str),
) -> Result<Option<Staged>, String> {
    let client = client()?;
    fetch_and_stage(target, known_version, cancelled, status, |url, limit| {
        download(&client, url, limit)
    })
}

fn fetch_and_stage(
    target: PathBuf,
    known_version: Option<Version>,
    cancelled: &AtomicBool,
    status: impl Fn(&str),
    mut fetch: impl FnMut(&str, u64) -> Result<Vec<u8>, String>,
) -> Result<Option<Staged>, String> {
    regular(&target)?;
    let target = target.canonicalize().map_err(err)?;
    let (arch, machine) = architecture()?;
    let releases = fetch(RELEASES, 2 * 1024 * 1024)?;
    let releases: Vec<Release> = serde_json::from_slice(&releases).map_err(err)?;
    check_cancelled(cancelled)?;
    let Some(package) = select(
        releases,
        &known_version.unwrap_or(Version::parse(env!("CARGO_PKG_VERSION")).map_err(err)?),
        arch,
    )?
    else {
        return Ok(None);
    };
    status(&format!("Downloading v{}...", package.version));
    let expected = checksum(
        &fetch(&package.checksum.browser_download_url, 1024)?,
        &package.archive.name,
    )?;
    check_cancelled(cancelled)?;
    let archive = fetch(&package.archive.browser_download_url, MAX_ARCHIVE)?;
    check_cancelled(cancelled)?;
    if archive.len() as u64 != package.archive.size || hash(&archive) != expected {
        return Err(
            "The downloaded archive failed SHA256/size verification. Nothing was installed.".into(),
        );
    }
    let exe = executable(&archive, machine)?;
    regular(&target)?;
    let directory = tempfile::Builder::new()
        .prefix(PREFIX)
        .tempdir_in(
            target
                .parent()
                .ok_or("The executable has no parent directory.")?,
        )
        .map_err(|e| {
            format!(
                "Cannot stage beside the portable executable. Move it to a writable folder: {e}"
            )
        })?;
    write_new(&directory.path().join("package.zip"), &archive)?;
    write_new(&directory.path().join("candidate.exe"), &exe)?;
    check_cancelled(cancelled)?;
    Ok(Some(Staged {
        directory,
        target,
        version: package.version,
        sha256: expected,
    }))
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::Relaxed) {
        Err("Automatic update check cancelled; automatic checks/downloads are off.".into())
    } else {
        Ok(())
    }
}

enum Event {
    Status(String),
    Checked(Result<Option<Staged>, String>),
    Installer(Result<PathBuf, String>),
}

pub struct Updates {
    pub status: String,
    pub staged: Option<Staged>,
    receiver: Option<Receiver<Event>>,
    next_check: Instant,
    last_check: Option<Instant>,
    failures: u32,
    pub installing: bool,
    cancellation: Arc<AtomicBool>,
    automatic_job: bool,
}

impl Updates {
    pub fn new() -> Self {
        Self {
            status: format!(
                "Installed v{}. Automatic check in 30 seconds.",
                env!("CARGO_PKG_VERSION")
            ),
            staged: None,
            receiver: None,
            next_check: Instant::now() + Duration::from_secs(30),
            last_check: None,
            failures: 0,
            installing: false,
            cancellation: Arc::new(AtomicBool::new(false)),
            automatic_job: false,
        }
    }

    pub fn busy(&self) -> bool {
        self.receiver.is_some()
    }

    pub fn set_automatic(&mut self, automatic: bool) {
        if !automatic && self.automatic_job {
            self.cancellation.store(true, Ordering::Relaxed);
        }
        if automatic {
            self.next_check = Instant::now() + Duration::from_secs(30);
        }
        if !self.busy() && self.staged.is_none() {
            self.status = format!(
                "Installed v{}. Automatic checks/downloads {}.",
                env!("CARGO_PKG_VERSION"),
                if automatic {
                    "enabled; check in 30 seconds"
                } else {
                    "disabled"
                }
            );
        }
    }

    pub fn check(&mut self, manual: bool) {
        if self.busy() || self.installing {
            return;
        }
        if self
            .last_check
            .is_some_and(|time| time.elapsed() < MANUAL_COOLDOWN)
        {
            if manual {
                self.status = "Please wait one minute between update checks.".into();
            }
            return;
        }
        self.last_check = Some(Instant::now());
        self.automatic_job = !manual;
        self.cancellation = Arc::new(AtomicBool::new(false));
        let cancelled = self.cancellation.clone();
        let known_version = self.staged.as_ref().map(|stage| stage.version.clone());
        self.status = "Checking public stable GitHub releases...".into();
        let (sender, receiver) = mpsc::channel();
        match thread::Builder::new()
            .name("update-check".into())
            .spawn(move || {
                let result = std::env::current_exe().map_err(err).and_then(|target| {
                    check_and_stage(target, known_version, &cancelled, |text| {
                        let _ = sender.send(Event::Status(text.into()));
                    })
                });
                let _ = sender.send(Event::Checked(result));
            }) {
            Ok(_) => self.receiver = Some(receiver),
            Err(error) => {
                self.status = format!("Cannot start update check: {error}");
                self.next_check = Instant::now() + Duration::from_secs(15 * 60);
            }
        }
    }

    // Returns a retained staging directory only once the helper has opened the
    // parent process handle and revalidated every byte. The UI can then exit.
    pub fn poll(&mut self, automatic: bool) -> Option<PathBuf> {
        if !automatic && self.automatic_job {
            self.cancellation.store(true, Ordering::Relaxed);
        }
        loop {
            let event = match self.receiver.as_ref().map(Receiver::try_recv) {
                Some(Ok(event)) => event,
                Some(Err(TryRecvError::Disconnected)) => {
                    self.receiver = None;
                    self.installing = false;
                    self.status =
                        "The update worker stopped unexpectedly. Try Check for updates again."
                            .into();
                    self.next_check = Instant::now() + Duration::from_secs(15 * 60);
                    break;
                }
                _ => break,
            };
            match event {
                Event::Status(text) => self.status = text,
                Event::Checked(result) => {
                    self.receiver = None;
                    if self.automatic_job && self.cancellation.load(Ordering::Relaxed) {
                        self.status =
                            "Automatic check cancelled. Manual checks remain available.".into();
                        self.next_check = Instant::now() + PERIOD;
                        break;
                    }
                    match result {
                        Ok(stage) => {
                            self.failures = 0;
                            if stage.is_some() {
                                self.staged = stage;
                            }
                            self.status = match &self.staged {
                                Some(stage) => format!(
                                    "v{} downloaded and verified. Install and restart only when you choose.",
                                    stage.version
                                ),
                                None => format!("v{} is up to date.", env!("CARGO_PKG_VERSION")),
                            };
                            self.next_check = Instant::now() + PERIOD;
                        }
                        Err(error) => {
                            self.failures = (self.failures + 1).min(8);
                            self.next_check = Instant::now()
                                + Duration::from_secs(
                                    (15 * 60 * (1u64 << (self.failures - 1))).min(24 * 60 * 60),
                                );
                            self.status = format!("Update failed: {error}");
                        }
                    }
                    break;
                }
                Event::Installer(result) => {
                    self.receiver = None;
                    match result {
                        Ok(path) => return Some(path),
                        Err(error) => {
                            self.installing = false;
                            self.status = format!("Cannot install update: {error}");
                        }
                    }
                    break;
                }
            }
        }
        if automatic && Instant::now() >= self.next_check && !self.busy() && !self.installing {
            self.check(false);
        }
        None
    }

    pub fn install(&mut self) {
        if self.busy() || self.installing {
            return;
        }
        let Some(stage) = self.staged.take() else {
            return;
        };
        let (sender, receiver) = mpsc::channel();
        self.installing = true;
        self.status = "Preparing verified installer. Waiting for the helper...".into();
        match thread::Builder::new()
            .name("update-install".into())
            .spawn(move || {
                let _ = sender.send(Event::Installer(launch_helper(stage)));
            }) {
            Ok(_) => self.receiver = Some(receiver),
            Err(error) => {
                self.installing = false;
                self.status = format!(
                    "Cannot start update installer: {error}. Check for updates to download again."
                );
            }
        }
    }
}

fn launch_helper(stage: Staged) -> Result<PathBuf, String> {
    let helper = stage.directory.path().join("helper.exe");
    fs::copy(std::env::current_exe().map_err(err)?, &helper).map_err(err)?;
    let mut child = Command::new(&helper)
        .arg("--apply-update")
        .arg(stage.directory.path())
        .arg(&stage.target)
        .arg(&stage.sha256)
        .arg(stage.version.to_string())
        .arg(std::process::id().to_string())
        .spawn()
        .map_err(err)?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if stage.directory.path().join("ready").exists() {
            return Ok(stage.directory.keep());
        }
        if child.try_wait().map_err(err)?.is_some() {
            let error_path = stage.directory.path().join("error.txt");
            if error_path.try_exists().map_err(err)? {
                regular(&error_path)?;
                let bytes = bounded_read(File::open(error_path).map_err(err)?, 16 * 1024)?;
                return Err(String::from_utf8(bytes).map_err(err)?);
            }
            return Err(
                "The installer helper exited before it was ready. Check for updates to retry."
                    .into(),
            );
        }
        if Instant::now() >= deadline {
            child.kill().map_err(err)?;
            child.wait().map_err(err)?;
            return Err("The installer helper timed out. Nothing was replaced.".into());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

struct Process(HANDLE);
impl Process {
    fn open(pid: u32) -> Result<Option<Self>, String> {
        if pid == 0 || pid == std::process::id() {
            return Err("Invalid updater process ID.".into());
        }
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if handle.is_null() {
            if unsafe { GetLastError() } == ERROR_INVALID_PARAMETER {
                return Ok(None);
            }
            return Err(format!(
                "Cannot wait for updater process: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(Some(Self(handle)))
    }
    fn wait(&self, timeout: u32) -> Result<(), String> {
        if unsafe { WaitForSingleObject(self.0, timeout) } != WAIT_OBJECT_0 {
            return Err(
                "Timed out waiting for the app to exit. Nothing further was replaced.".into(),
            );
        }
        Ok(())
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn scope(stage: &Path, target: &Path) -> Result<(), String> {
    use std::os::windows::fs::MetadataExt;
    if !stage.is_absolute() || !target.is_absolute() {
        return Err("Update paths must be absolute.".into());
    }
    let metadata = fs::symlink_metadata(stage).map_err(err)?;
    if !metadata.is_dir()
        || metadata.file_attributes() & 0x400 != 0
        || !stage
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(PREFIX))
        || stage.canonicalize().map_err(err)? != stage
        || target
            .parent()
            .ok_or("Missing target parent.")?
            .canonicalize()
            .map_err(err)?
            != stage.parent().ok_or("Missing stage parent.")?
        || target.file_name().is_none()
        || target
            .extension()
            .is_none_or(|ext| !ext.eq_ignore_ascii_case("exe"))
    {
        return Err("The updater staging directory is outside its permitted scope.".into());
    }
    Ok(())
}

fn revalidate(stage: &Path, expected: &str, machine: u16) -> Result<(), String> {
    let archive_path = stage.join("package.zip");
    let candidate = stage.join("candidate.exe");
    regular(&archive_path)?;
    regular(&candidate)?;
    let archive = bounded_read(File::open(archive_path).map_err(err)?, MAX_ARCHIVE)?;
    if hash(&archive) != expected {
        return Err("The staged ZIP changed after download. Nothing was installed.".into());
    }
    let bytes = executable(&archive, machine)?;
    if bounded_read(File::open(candidate).map_err(err)?, MAX_EXE)? != bytes {
        return Err(
            "The staged executable changed after verification. Nothing was installed.".into(),
        );
    }
    Ok(())
}

fn rollback(target: &Path, backup: &Path) -> Result<(), String> {
    if target.exists() {
        regular(target)?;
        fs::remove_file(target).map_err(err)?;
    }
    fs::rename(backup, target)
        .map_err(|e| format!("Rollback failed; original is at {}: {e}", backup.display()))
}

fn replace(
    stage: &Path,
    target: &Path,
    restart: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let backup = stage.join("backup.exe");
    if backup.exists() {
        return Err("An update backup already exists; refusing to overwrite it.".into());
    }
    regular(target)?;
    fs::rename(target, &backup).map_err(|e| format!("Cannot replace the portable executable. Check folder permissions or other running copies: {e}"))?;
    let result = fs::rename(stage.join("candidate.exe"), target)
        .map_err(err)
        .and_then(|()| restart());
    if let Err(error) = result {
        rollback(target, &backup)?;
        return Err(format!("The original executable was restored: {error}"));
    }
    Ok(())
}

fn restart_updated(stage: &Path, target: &Path) -> Result<(), String> {
    let mut command = Command::new(target);
    command
        .arg("--update-started")
        .arg(stage)
        .arg(std::process::id().to_string());
    restart_command(stage, &mut command)
}

fn restart_command(stage: &Path, command: &mut Command) -> Result<(), String> {
    let mut child = command.spawn().map_err(err)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let result = (|| loop {
        if stage.join("started").exists() {
            return Ok(());
        }
        if child.try_wait().map_err(err)?.is_some() {
            return Err("The updated application exited before initializing.".into());
        }
        if Instant::now() >= deadline {
            return Err("The updated application did not initialize in time.".into());
        }
        thread::sleep(Duration::from_millis(100));
    })();
    if result.is_err() {
        stop_child(&mut child)?;
    }
    result
}

fn stop_child(child: &mut Child) -> Result<(), String> {
    if child.try_wait().map_err(err)?.is_none() {
        child.kill().map_err(err)?;
    }
    child.wait().map_err(err)?;
    Ok(())
}

fn apply(args: &[OsString]) -> Result<(), String> {
    apply_with_restart(args, restart_updated)
}

fn apply_with_restart(
    args: &[OsString],
    restart: impl FnOnce(&Path, &Path) -> Result<(), String>,
) -> Result<(), String> {
    if args.len() != 7 {
        return Err("Invalid update-helper arguments.".into());
    }
    let stage = PathBuf::from(&args[2]);
    let target = PathBuf::from(&args[3]);
    scope(&stage, &target)?;
    if std::env::current_exe()
        .map_err(err)?
        .canonicalize()
        .map_err(err)?
        != stage.join("helper.exe")
    {
        return Err("The update helper must run from its staging directory.".into());
    }
    let version =
        Version::parse(args[5].to_str().ok_or("Invalid update version.")?).map_err(err)?;
    let current = Version::parse(env!("CARGO_PKG_VERSION")).map_err(err)?;
    if !version.pre.is_empty() || !version.cmp_precedence(&current).is_gt() {
        return Err("Refusing to install a prerelease or downgrade.".into());
    }
    let expected = args[4].to_str().ok_or("Invalid checksum.")?;
    let pid = args[6]
        .to_str()
        .ok_or("Invalid process ID.")?
        .parse()
        .map_err(err)?;
    let parent = Process::open(pid)?.ok_or("The parent app is no longer running.")?;
    revalidate(&stage, expected, architecture()?.1)?;
    write_new(&stage.join("ready"), b"")?;
    parent.wait(120_000)?;
    let result = revalidate(&stage, expected, architecture()?.1)
        .and_then(|()| replace(&stage, &target, || restart(&stage, &target)));
    if let Err(error) = result {
        if target.exists() && !stage.join("backup.exe").exists() {
            Command::new(&target)
                .spawn()
                .map_err(|e| format!("{error}; could not restart original: {e}"))?;
        }
        return Err(error);
    }
    Ok(())
}

pub fn helper_from_args() -> Option<Result<(), String>> {
    let args: Vec<_> = std::env::args_os().collect();
    (args.get(1).is_some_and(|arg| arg == "--apply-update")).then(|| apply(&args))
}

pub fn report_helper_error(error: &str) {
    let args: Vec<_> = std::env::args_os().collect();
    let saved = (|| -> Result<bool, String> {
        if args.len() != 7 {
            return Err("Invalid helper error destination.".into());
        }
        let stage = PathBuf::from(&args[2]);
        let target = PathBuf::from(&args[3]);
        scope(&stage, &target)?;
        if std::env::current_exe()
            .map_err(err)?
            .canonicalize()
            .map_err(err)?
            != stage.join("helper.exe")
        {
            return Err("Invalid helper location.".into());
        }
        write_new(&stage.join("error.txt"), error.as_bytes())?;
        stage.join("ready").try_exists().map_err(err)
    })();
    // Before readiness, the still-running parent reads the error without a
    // second dialog. After handoff, the helper must surface failures itself.
    match saved {
        Ok(false) => {}
        Ok(true) => {
            crate::desktop::report_error(&format!("Update could not be installed.\r\n\r\n{error}"))
        }
        Err(report_error) => crate::desktop::report_error(&format!(
            "Update could not be installed.\r\n\r\n{error}\r\n\r\nCould not record helper status: {report_error}"
        )),
    }
}

fn clean(stage: &Path, target: &Path) -> Result<(), String> {
    scope(stage, target)?;
    // Never recursively delete a path supplied on the command line.
    for name in [
        "package.zip",
        "candidate.exe",
        "helper.exe",
        "ready",
        "started",
        "backup.exe",
        "error.txt",
    ] {
        let path = stage.join(name);
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                regular(&path)?;
                fs::remove_file(path).map_err(err)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(err(error)),
        }
    }
    fs::remove_dir(stage).map_err(err)
}

// The desktop calls this only after native initialization succeeds.
pub fn finish_restart() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().collect();
    finish_restart_args(&args)
}

fn finish_restart_args(args: &[OsString]) -> Result<(), String> {
    if !args.get(1).is_some_and(|arg| arg == "--update-started") {
        return Ok(());
    }
    if args.len() != 4 {
        return Err("Invalid update-restart arguments.".into());
    }
    let stage = PathBuf::from(&args[2]);
    let target = std::env::current_exe()
        .map_err(err)?
        .canonicalize()
        .map_err(err)?;
    scope(&stage, &target)?;
    let pid = args[3]
        .to_str()
        .ok_or("Invalid helper process ID.")?
        .parse()
        .map_err(err)?;
    let helper = Process::open(pid)?;
    write_new(&stage.join("started"), b"")?;
    // The helper's own executable remains locked until it exits.
    thread::Builder::new()
        .name("update-cleanup".into())
        .spawn(move || {
            let result = helper
                .map_or(Ok(()), |p| p.wait(120_000))
                .and_then(|()| clean(&stage, &target));
            if let Err(error) = result {
                crate::desktop::report_error(&format!(
                    "Update installed, but cleanup needs attention at {}:\r\n{error}",
                    stage.display()
                ));
            }
        })
        .map_err(err)?;
    Ok(())
}

// A process handle is owned by this value and used only for waiting/closing.
unsafe impl Send for Process {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{collections::HashMap, net::TcpListener, process::Stdio};
    use zip::write::SimpleFileOptions;

    fn pe(machine: u16) -> Vec<u8> {
        let mut bytes = vec![0; 256];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&128u32.to_le_bytes());
        bytes[128..132].copy_from_slice(b"PE\0\0");
        bytes[132..134].copy_from_slice(&machine.to_le_bytes());
        bytes[150..152].copy_from_slice(&0x0002u16.to_le_bytes());
        bytes[152..154].copy_from_slice(&0x020bu16.to_le_bytes());
        bytes
    }

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn release(tag: &str, arch: &str, size: usize) -> serde_json::Value {
        let name = format!("cursor-highlighter-windows-{arch}.zip");
        json!({
            "tag_name": tag, "draft": false, "prerelease": false,
            "assets": [
                {"name": name, "size": size,
                 "browser_download_url": format!("https://github.com/{REPOSITORY}/releases/download/{tag}/{name}")},
                {"name": format!("{name}.sha256"), "size": 102,
                 "browser_download_url": format!("https://github.com/{REPOSITORY}/releases/download/{tag}/{name}.sha256")}
            ]
        })
    }

    fn select_values(
        values: Vec<serde_json::Value>,
        current: &str,
        arch: &str,
    ) -> Result<Option<Package>, String> {
        select(
            serde_json::from_value(json!(values)).unwrap(),
            &Version::parse(current).unwrap(),
            arch,
        )
    }

    #[test]
    fn versions_are_semver_stable_and_never_downgrade() {
        let mut draft = release("v99.0.0", "arm64", 256);
        draft["draft"] = json!(true);
        let mut prerelease = release("v98.0.0", "arm64", 256);
        prerelease["prerelease"] = json!(true);
        let values = vec![
            release("v0.9.0", "arm64", 256),
            release("v0.10.0", "arm64", 256),
            release("v5.0.0-beta.1", "arm64", 256),
            release("invalid", "arm64", 256),
            draft,
            prerelease,
        ];
        assert_eq!(
            select_values(values, "0.2.0", "arm64")
                .unwrap()
                .unwrap()
                .version,
            Version::parse("0.10.0").unwrap()
        );
        for version in ["v0.1.0", "v0.2.0", "v0.2.0+build.99"] {
            assert!(
                select_values(vec![release(version, "x64", 256)], "0.2.0", "x64")
                    .unwrap()
                    .is_none()
            );
        }
        assert!(select_values(vec![release("v0.3.0", "x64", 256)], "0.2.0", "arm64").is_err());
        assert!(
            select_values(vec![release("v0.3.0", "arm64", 256)], "0.2.0", "arm64")
                .unwrap()
                .is_some()
        );
        assert!(
            select_values(
                vec![release("v0.3.0", "x64", MAX_ARCHIVE as usize + 1)],
                "0.2.0",
                "x64"
            )
            .is_err()
        );
    }

    #[test]
    fn asset_urls_and_redirects_stay_in_trusted_https_scope() {
        for url in [
            "http://github.com/a",
            "https://evil.example/a",
            "https://github.com.evil.example/a",
            "https://github.com@evil.example/a",
            "https://github.com:444/a",
            "https://user@github.com/a",
        ] {
            assert!(!trusted_redirect(&Url::parse(url).unwrap()), "{url}");
        }
        for host in [
            "github.com",
            "release-assets.githubusercontent.com",
            "objects.githubusercontent.com",
        ] {
            assert!(trusted_redirect(
                &Url::parse(&format!("https://{host}/a")).unwrap()
            ));
        }
        let good = release("v0.3.0", "x64", 256);
        for url in [
            "https://github.com/other/repo/releases/download/v0.3.0/cursor-highlighter-windows-x64.zip",
            "https://github.com/qbq-hh-sebastian-meyer/cursor-highlighter/releases/download/v0.3.0/cursor-highlighter-windows-x64.zip?redirect=evil",
            "http://github.com/qbq-hh-sebastian-meyer/cursor-highlighter/releases/download/v0.3.0/cursor-highlighter-windows-x64.zip",
        ] {
            let mut bad = good.clone();
            bad["assets"][0]["browser_download_url"] = json!(url);
            assert!(select_values(vec![bad], "0.2.0", "x64").is_err());
        }
        let mut duplicate = good.clone();
        duplicate["assets"]
            .as_array_mut()
            .unwrap()
            .push(good["assets"][0].clone());
        assert!(select_values(vec![duplicate], "0.2.0", "x64").is_err());
    }

    #[test]
    fn checksum_and_zip_reject_corruption_traversal_duplicates_and_wrong_machine() {
        let exe = pe(0xAA64);
        let zip = archive(&[("cursor-highlighter.exe", &exe), ("README.md", b"readme")]);
        assert_eq!(executable(&zip, 0xAA64).unwrap(), exe);
        assert!(executable(&zip, 0x8664).is_err());
        for bytes in [&b"not a zip"[..], &zip[..zip.len() / 2]] {
            assert!(executable(bytes, 0xAA64).is_err());
        }
        for name in [
            "../cursor-highlighter.exe",
            "C:\\evil.exe",
            "folder/cursor-highlighter.exe",
            "folder\\cursor-highlighter.exe",
            "/cursor-highlighter.exe",
            "unknown.txt",
        ] {
            assert!(
                executable(&archive(&[(name, &exe)]), 0xAA64).is_err(),
                "{name}"
            );
        }
        assert!(executable(&archive(&[("README.md", b"only readme")]), 0xAA64).is_err());
        let mut duplicate = zip::ZipWriter::new(Cursor::new(Vec::new()));
        duplicate
            .add_symlink(
                "cursor-highlighter.exe",
                "elsewhere",
                SimpleFileOptions::default(),
            )
            .unwrap();
        assert!(executable(&duplicate.finish().unwrap().into_inner(), 0xAA64).is_err());
        let good = format!("{}  package.zip\r\n", hash(&zip).to_uppercase());
        assert_eq!(
            checksum(good.as_bytes(), "package.zip").unwrap(),
            hash(&zip)
        );
        for text in [
            "bad package.zip".to_string(),
            format!("{} other.zip", hash(&zip)),
            format!("{} package.zip extra", hash(&zip)),
        ] {
            assert!(checksum(text.as_bytes(), "package.zip").is_err());
        }
        let mut bad_pe = exe.clone();
        bad_pe[0x3c..0x40].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(validate_pe(&bad_pe, 0xAA64).is_err());
        assert!(bounded_read(Cursor::new(vec![0; 17]), 16).is_err());
    }

    fn fixtures(root: &Path) -> (PathBuf, HashMap<String, Vec<u8>>) {
        let target = root.join("renamed portable app.exe");
        write_new(&target, b"old executable").unwrap();
        let (arch, machine) = architecture().unwrap();
        let zip = archive(&[("cursor-highlighter.exe", &pe(machine))]);
        let release = release("v999.0.0", arch, zip.len());
        let mut responses = HashMap::new();
        responses.insert(
            RELEASES.into(),
            serde_json::to_vec(&json!([release.clone()])).unwrap(),
        );
        responses.insert(
            release["assets"][0]["browser_download_url"]
                .as_str()
                .unwrap()
                .into(),
            zip.clone(),
        );
        responses.insert(
            release["assets"][1]["browser_download_url"]
                .as_str()
                .unwrap()
                .into(),
            format!("{}  cursor-highlighter-windows-{arch}.zip\n", hash(&zip)).into_bytes(),
        );
        (target, responses)
    }

    #[test]
    fn mocked_end_to_end_download_stages_only_verified_bytes_and_cleans_on_drop() {
        let root = tempfile::tempdir().unwrap();
        let (target, responses) = fixtures(root.path());
        let stage = fetch_and_stage(
            target.clone(),
            None,
            &AtomicBool::new(false),
            |_| {},
            |url, limit| {
                let bytes = responses.get(url).ok_or("Missing fixture")?;
                bounded_read(Cursor::new(bytes), limit)
            },
        )
        .unwrap()
        .unwrap();
        let directory = stage.directory.path().to_path_buf();
        scope(&directory, &stage.target).unwrap();
        assert_eq!(stage.target.file_name(), target.file_name());
        revalidate(&directory, &stage.sha256, architecture().unwrap().1).unwrap();
        fs::write(directory.join("candidate.exe"), b"changed").unwrap();
        assert!(revalidate(&directory, &stage.sha256, architecture().unwrap().1).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"old executable");
        drop(stage);
        assert!(!directory.exists());
        assert_eq!(fs::read(target).unwrap(), b"old executable");
    }

    #[test]
    fn mocked_network_bad_json_checksum_archive_and_cancellation_never_replace_target() {
        for mode in ["network", "json", "checksum", "zip", "cancel"] {
            let root = tempfile::tempdir().unwrap();
            let (target, mut responses) = fixtures(root.path());
            if mode == "json" {
                responses.insert(RELEASES.into(), b"{".to_vec());
            }
            if mode == "checksum" {
                let url = responses
                    .keys()
                    .find(|url| url.ends_with(".sha256"))
                    .unwrap()
                    .clone();
                responses.insert(url, b"bad checksum".to_vec());
            }
            if mode == "zip" {
                let url = responses
                    .keys()
                    .find(|url| url.ends_with(".zip"))
                    .unwrap()
                    .clone();
                responses.get_mut(&url).unwrap()[0] ^= 0xFF;
            }
            let result = fetch_and_stage(
                target.clone(),
                None,
                &AtomicBool::new(mode == "cancel"),
                |_| {},
                |url, _| {
                    if mode == "network" {
                        return Err("HTTP 429 fixture".into());
                    }
                    responses.get(url).cloned().ok_or("Missing fixture".into())
                },
            );
            assert!(result.is_err(), "{mode}");
            assert_eq!(fs::read(target).unwrap(), b"old executable");
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn local_http_errors_limits_and_timeouts_are_actionable() {
        for (response, expected) in [
            (
                "HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\n\r\n",
                "429",
            ),
            ("HTTP/1.1 200 OK\r\nContent-Length: 9999\r\n\r\n", "size"),
            ("HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc", "abc"),
            ("", "timed out"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/fixture", listener.local_addr().unwrap());
            let server = thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = [0; 4096];
                assert!(socket.read(&mut request).unwrap() > 0);
                if response.is_empty() {
                    thread::sleep(Duration::from_millis(300));
                } else {
                    socket.write_all(response.as_bytes()).unwrap();
                }
            });
            let client = Client::builder()
                .no_proxy()
                .timeout(Duration::from_millis(150))
                .build()
                .unwrap();
            let result = download(&client, &url, 16);
            if expected == "abc" {
                assert_eq!(result.unwrap(), b"abc");
            } else {
                assert!(result.unwrap_err().contains(expected));
            }
            server.join().unwrap();
        }
        assert!(client().unwrap().get("http://127.0.0.1/").send().is_err());
    }

    #[test]
    fn isolated_replacement_rolls_back_on_rename_and_restart_failure() {
        for mode in ["success", "missing", "restart"] {
            let root = tempfile::tempdir().unwrap();
            let target = root.path().join("portable.exe");
            let stage = tempfile::Builder::new()
                .prefix(PREFIX)
                .tempdir_in(root.path())
                .unwrap();
            fs::write(&target, b"old").unwrap();
            if mode != "missing" {
                fs::write(stage.path().join("candidate.exe"), b"new").unwrap();
            }
            let result = replace(stage.path(), &target, || {
                assert_eq!(fs::read(&target).unwrap(), b"new");
                assert_eq!(fs::read(stage.path().join("backup.exe")).unwrap(), b"old");
                if mode == "restart" {
                    Err("simulated initialization failure".into())
                } else {
                    Ok(())
                }
            });
            assert_eq!(result.is_ok(), mode == "success");
            assert_eq!(
                fs::read(&target).unwrap(),
                if mode == "success" {
                    &b"new"[..]
                } else {
                    &b"old"[..]
                }
            );
        }
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("portable.exe");
        fs::write(&target, b"old").unwrap();
        fs::write(root.path().join("backup.exe"), b"existing").unwrap();
        assert!(replace(root.path(), &target, || Ok(())).is_err());
        assert_eq!(fs::read(target).unwrap(), b"old");
        assert_eq!(
            fs::read(root.path().join("backup.exe")).unwrap(),
            b"existing"
        );
    }

    #[test]
    fn replacement_permission_failure_preserves_original_and_staging_scope_is_strict() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("portable.exe");
        fs::write(&target, b"old").unwrap();
        let stage = tempfile::Builder::new()
            .prefix(PREFIX)
            .tempdir_in(root.path())
            .unwrap();
        fs::write(stage.path().join("candidate.exe"), b"new").unwrap();
        let locked = File::options()
            .read(true)
            .share_mode(0)
            .open(&target)
            .unwrap();
        assert!(
            replace(stage.path(), &target, || Ok(()))
                .unwrap_err()
                .contains("permissions")
        );
        drop(locked);
        assert_eq!(fs::read(&target).unwrap(), b"old");
        assert!(scope(root.path(), &target).is_err());
        assert!(
            scope(
                &stage.path().canonicalize().unwrap(),
                &root.path().join("other").join("target.exe")
            )
            .is_err()
        );
        let canonical_target = target.canonicalize().unwrap();
        let canonical_stage = stage.path().canonicalize().unwrap();
        scope(&canonical_stage, &canonical_target).unwrap();
        fs::write(stage.path().join("unrelated.txt"), b"must survive").unwrap();
        assert!(clean(&canonical_stage, &canonical_target).is_err());
        assert_eq!(
            fs::read(stage.path().join("unrelated.txt")).unwrap(),
            b"must survive"
        );
        assert_eq!(fs::read(target).unwrap(), b"old");
    }

    #[test]
    fn polling_opt_out_cooldown_backoff_and_worker_disconnect_are_bounded() {
        let mut updates = Updates::new();
        updates.next_check = Instant::now() - Duration::from_secs(1);
        updates.set_automatic(false);
        assert!(updates.poll(false).is_none());
        assert!(!updates.busy());
        updates.last_check = Some(Instant::now());
        updates.check(true);
        assert!(updates.status.contains("one minute"));
        for failure in 1..=8 {
            let (sender, receiver) = mpsc::channel();
            updates.receiver = Some(receiver);
            sender.send(Event::Checked(Err("offline".into()))).unwrap();
            let before = Instant::now();
            updates.poll(false);
            let delay = updates.next_check.duration_since(before).as_secs();
            assert!((899..=86400).contains(&delay), "{failure}: {delay}");
        }
        let (sender, receiver) = mpsc::channel();
        updates.receiver = Some(receiver);
        drop(sender);
        updates.poll(false);
        assert!(updates.status.contains("unexpectedly"));
        assert!(!updates.installing);
        updates.automatic_job = true;
        updates.set_automatic(false);
        assert!(updates.cancellation.load(Ordering::Relaxed));
    }

    #[test]
    fn settings_additive_opt_out_roundtrips_and_presets_preserve_it() {
        use crate::model::{PresentationPreset, Settings};
        let mut settings: Settings = serde_json::from_str(r#"{"Diameter":90}"#).unwrap();
        assert!(settings.automatic_updates);
        settings.automatic_updates = false;
        settings.apply_preset(PresentationPreset::Slides);
        assert!(!settings.automatic_updates);
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("settings.json");
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), settings);
        assert!(serde_json::from_str::<Settings>(r#"{"AutomaticUpdates":"no"}"#).is_err());
    }

    fn driver_command(
        exe: &Path,
        mode: &str,
        stage: &Path,
        target: &Path,
        digest: &str,
    ) -> Command {
        let mut command = Command::new(exe);
        command
            .args([
                "--exact",
                "updater::tests::process_driver",
                "--ignored",
                "--nocapture",
            ])
            .env("CURSOR_UPDATER_TEST_MODE", mode)
            .env("CURSOR_UPDATER_TEST_STAGE", stage)
            .env("CURSOR_UPDATER_TEST_TARGET", target)
            .env("CURSOR_UPDATER_TEST_HASH", digest)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    #[test]
    #[ignore = "Invoked only as a scoped child by real_helper_process_handoff_and_cleanup"]
    #[allow(
        clippy::zombie_processes,
        reason = "The fixture parent must exit before its helper can continue."
    )]
    fn process_driver() {
        let mode = std::env::var("CURSOR_UPDATER_TEST_MODE").unwrap();
        let stage = PathBuf::from(std::env::var_os("CURSOR_UPDATER_TEST_STAGE").unwrap());
        let target = PathBuf::from(std::env::var_os("CURSOR_UPDATER_TEST_TARGET").unwrap());
        let digest = std::env::var("CURSOR_UPDATER_TEST_HASH").unwrap();
        match mode.as_str() {
            "parent" => {
                let helper = stage.join("helper.exe");
                fs::copy(std::env::current_exe().unwrap(), &helper).unwrap();
                let mut child = driver_command(&helper, "helper", &stage, &target, &digest)
                    .env("CURSOR_UPDATER_TEST_PARENT", std::process::id().to_string())
                    .spawn()
                    .unwrap();
                let deadline = Instant::now() + Duration::from_secs(20);
                while !stage.join("ready").exists() {
                    assert!(
                        child.try_wait().unwrap().is_none(),
                        "helper exited before readiness"
                    );
                    assert!(Instant::now() < deadline, "helper never became ready");
                    thread::sleep(Duration::from_millis(50));
                }
                assert!(stage.join("candidate.exe").exists());
                assert!(
                    !stage.join("backup.exe").exists(),
                    "helper replaced a still-running parent"
                );
            }
            "helper" => {
                let args = vec![
                    OsString::from("fixture"),
                    OsString::from("--apply-update"),
                    stage.clone().into_os_string(),
                    target.clone().into_os_string(),
                    digest.clone().into(),
                    "999.0.0".into(),
                    std::env::var_os("CURSOR_UPDATER_TEST_PARENT").unwrap(),
                ];
                apply_with_restart(&args, |stage, target| {
                    restart_command(
                        stage,
                        driver_command(target, "updated", stage, target, &digest)
                            .env("CURSOR_UPDATER_TEST_PARENT", std::process::id().to_string()),
                    )
                })
                .unwrap();
            }
            "updated" => {
                let args = vec![
                    OsString::from("fixture"),
                    OsString::from("--update-started"),
                    stage.clone().into_os_string(),
                    std::env::var_os("CURSOR_UPDATER_TEST_PARENT").unwrap(),
                ];
                finish_restart_args(&args).unwrap();
                let deadline = Instant::now() + Duration::from_secs(20);
                while stage.exists() {
                    assert!(Instant::now() < deadline, "cleanup never completed");
                    thread::sleep(Duration::from_millis(50));
                }
                write_new(&target.with_extension("done"), b"initialized and cleaned").unwrap();
            }
            _ => panic!("unexpected fixture mode"),
        }
    }

    #[test]
    fn real_helper_process_handoff_and_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let directory = tempfile::Builder::new()
            .prefix(PREFIX)
            .tempdir_in(root.path())
            .unwrap();
        let stage = directory.path().canonicalize().unwrap();
        let target = root
            .path()
            .canonicalize()
            .unwrap()
            .join("portable & \u{00fc} fixture.exe");
        let test_exe = std::env::current_exe().unwrap();
        fs::copy(&test_exe, &target).unwrap();
        let candidate = fs::read(&test_exe).unwrap();
        let zip = archive(&[("cursor-highlighter.exe", &candidate)]);
        write_new(&stage.join("package.zip"), &zip).unwrap();
        write_new(&stage.join("candidate.exe"), &candidate).unwrap();
        let mut parent = driver_command(&test_exe, "parent", &stage, &target, &hash(&zip))
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(45);
        while !target.with_extension("done").exists() {
            if Instant::now() >= deadline {
                stop_child(&mut parent).unwrap();
                panic!("isolated helper handoff did not complete");
            }
            thread::sleep(Duration::from_millis(100));
        }
        assert!(parent.wait().unwrap().success());
        assert!(!stage.exists());
        assert_eq!(fs::read(&target).unwrap(), candidate);
        // Wait for only the test-owned restarted process to release its image.
        let deadline = Instant::now() + Duration::from_secs(5);
        while fs::remove_file(&target).is_err() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn downloaded_update_waits_for_consent_and_survives_subsequent_checks() {
        let root = tempfile::tempdir().unwrap();
        let (target, responses) = fixtures(root.path());
        let stage = fetch_and_stage(
            target.clone(),
            None,
            &AtomicBool::new(false),
            |_| {},
            |url, _| responses.get(url).cloned().ok_or("Missing fixture".into()),
        )
        .unwrap()
        .unwrap();
        let mut requests = 0;
        let nothing_newer = fetch_and_stage(
            target.clone(),
            Some(stage.version.clone()),
            &AtomicBool::new(false),
            |_| {},
            |url, _| {
                requests += 1;
                responses.get(url).cloned().ok_or("Missing fixture".into())
            },
        )
        .unwrap();
        assert!(nothing_newer.is_none());
        assert_eq!(
            requests, 1,
            "an already staged version must not be downloaded again"
        );
        let mut updates = Updates::new();
        updates.staged = Some(stage);
        for result in [Ok(None), Err("offline".into())] {
            let (sender, receiver) = mpsc::channel();
            updates.receiver = Some(receiver);
            sender.send(Event::Checked(result)).unwrap();
            assert!(updates.poll(true).is_none());
            assert!(updates.staged.is_some());
            assert!(!updates.installing);
            assert_eq!(fs::read(&target).unwrap(), b"old executable");
        }
    }
}
