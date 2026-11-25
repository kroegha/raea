//! A local model's install on any OS, with nothing from Homebrew or the system's package manager: its
//! tools/<model>/setup.py, run by uv (which makes the venv and brings its own Python), and the venv's Python run after.
//! uv is the one on the PATH, else raea's own: a pinned release fetched once into ~/.cache/raea-tools/uv and checked
//! against its SHA-256 (where tools/ensure.sh puts the same uv on a Mac, so either finds the other's). Until 0.1.8 each
//! setup was a `sh` script needing uv already installed (`brew install uv`), so Windows had no way to install any local
//! model, and a Mac without Homebrew none either. FFmpeg, on Windows (ffmpeg_tool), is fetched the same way the first
//! time a video needs it.

use super::*;

/// The uv raea fetches when there's none on the PATH: the release every local model's setup was measured with
const UV: &str = "0.12.24";

/// uv's release archive for each platform raea ships on, by target, and its SHA-256 (the release's sha256.sum)
const UV_ARCHIVES: [(&str, &str); 6] = [
    ("aarch64-apple-darwin", "0c4346de7abdb49495b393b9ec809fe387aa43e586be20fecb972216c1e71732"),
    ("x86_64-apple-darwin", "4fa82e37cb94767661f532b001e470b67a186c7260e305bd84ddb78fd545c0b6"),
    ("aarch64-unknown-linux-gnu", "5231be65f496304623895dacdbf1de8504fec90303684bdf05805aa34414dd21"),
    ("x86_64-unknown-linux-gnu", "b4dfaef47d491a7296981f8374a4595f55dbf84e8937c8ecd2983574d8bb3da6"),
    ("aarch64-pc-windows-msvc", "4b783bda5cc44bbae0651a837223873a7acee31152381aef5fc86ef6bef25997"),
    ("x86_64-pc-windows-msvc", "7c38608c8a18ee137d748a1773053b07ec8f3a30fab49aebaa6f4e4efeceb019"),
];

fn exe(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_string() }
}

/// Every copy of `name` raea could run, in ffmpeg_tool's order and without fetching: each one on the PATH, the one
/// beside raea's executable, then the one raea fetched earlier. For a choice between them (an encoder: the one whose
/// ffmpeg encodes on the GPU, whichever PATH lists first).
pub fn tool_copies(name: &str) -> Vec<PathBuf> {
    let exe = exe(name);
    let mut all: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).map(|d| d.join(&exe)).filter(|p| p.is_file()).collect()).unwrap_or_default();
    all.extend(beside_exe(name));
    let cached = raea_io_home::env_path("RAEA_TOOLS_CACHE", ".cache/raea-tools").join("ffmpeg").join("bin").join(&exe);
    all.extend(Some(cached).filter(|p| p.is_file()));
    let mut seen = std::collections::HashSet::new();
    all.retain(|p| seen.insert(std::fs::canonicalize(p).unwrap_or_else(|_| p.clone())));
    all
}

fn on_path(name: &str) -> Option<PathBuf> {
    let exe = exe(name);
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(&exe)).find(|p| p.is_file())
}

/// The tool shipped beside raea's own executable: on Windows the release puts FFmpeg and uv in raea.exe's bin/// (tools/release/win-bundle.sh), so raea finds them however it was started and fetches nothing
fn beside_exe(name: &str) -> Option<PathBuf> {
    beside(&std::env::current_exe().ok()?, name)
}

fn beside(exe_path: &Path, name: &str) -> Option<PathBuf> {
    Some(exe_path.parent()?.join(exe(name))).filter(|p| p.is_file())
}

/// The FFmpeg raea ships beside raea.exe on Windows and fetches where it isn't: raea's own build of FFmpeg's release,
/// LGPL 2.1 or later: FFmpeg's own code, zlib and libvpx's decoders, its H.264 encoder Windows' own (Media
/// Foundation) (tools/release/ffmpeg-win.sh, published with its source as the prerelease tools-ffmpeg-9.0.2-lgpl). Until 0.1.8 raea fetched GyanD's essentials build, GPLv3 with GPL libraries
/// linked in, which raea couldn't ship. tools/release/win-bundle.sh reads these pins.
const FFMPEG: &str = "9.0.2";
const FFMPEG_TOP: &str = "ffmpeg-9.0.2-lgpl-win64";
const FFMPEG_URL: &str = "https://github.com/shinyobjectz/raea/releases/download/tools-ffmpeg-9.0.2-lgpl/ffmpeg-9.0.2-lgpl-win64.zip";
const FFMPEG_SUM: &str = "d3a3d8cdcd3ee488a94328200293a312e9c81923902b9fc3c9fa5dd18779ba8c";

/// `ffmpeg` or `ffprobe` as raea runs it: the one on the PATH; else the one beside raea's executable (the Windows
/// release ships it there); else raea's own in $RAEA_TOOLS_CACHE (else ~/.cache/raea-tools)/ffmpeg/bin. On Windows,
/// where FFmpeg is rarely installed, raea's own is fetched there once the first time a video raea's own decoders
/// can't read needs it, when `fetch`, if the release's copy is missing: H.264 with B-frames, HEVC, VP9 (a phone's or a
/// camera's). Elsewhere FFmpeg is the system's: a Mac reads those on VideoToolbox, and Linux has FFmpeg in its package
/// manager.
pub fn ffmpeg_tool(name: &str, fetch: bool) -> Result<PathBuf, String> {
    if let Some(p) = on_path(name).or_else(|| beside_exe(name)) {
        return Ok(p);
    }
    let dir = raea_io_home::env_path("RAEA_TOOLS_CACHE", ".cache/raea-tools").join("ffmpeg");
    let bin = dir.join("bin").join(exe(name));
    if bin.is_file() {
        return Ok(bin);
    }
    if !cfg!(windows) {
        return Err(format!("{name} isn't installed: raea reads this video through FFmpeg (apt install ffmpeg, dnf install ffmpeg or brew install ffmpeg)"));
    }
    if !fetch {
        return Err(format!("{name} isn't installed yet (raea fetches its own the first time a video needs it)"));
    }
    // one fetch at a time in a process (a render opens its videos on several threads); another process's is a
    // separate .part folder, and whichever finishes first is moved in
    static FETCHING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = FETCHING.lock().unwrap_or_else(|e| e.into_inner());
    if bin.is_file() {
        return Ok(bin);
    }
    raea_io_home::offline(&format!("fetching FFmpeg {FFMPEG} to read a video raea's own decoders can't"))?;
    eprintln!("raea: this video needs FFmpeg, which isn't installed; fetching FFmpeg {FFMPEG} (once) into {}", dir.display());
    let name_zip = format!("{FFMPEG_TOP}.zip");
    let members = [format!("{FFMPEG_TOP}/bin/ffmpeg.exe"), format!("{FFMPEG_TOP}/bin/ffprobe.exe"), format!("{FFMPEG_TOP}/LICENSE")];
    let part = dir.with_file_name(format!("ffmpeg.part{}", std::process::id()));
    let fetched = fetch_archive(&part, FFMPEG_URL, &name_zip, FFMPEG_SUM, true, &members.each_ref().map(String::as_str));
    settle(&part, &dir, &bin, fetched, "FFmpeg")
}

/// The Python of the venv in an install folder: .venv/bin/python, or .venv\Scripts\python.exe on Windows
pub fn venv_python(dir: &Path) -> PathBuf {
    if cfg!(windows) { dir.join(".venv").join("Scripts").join("python.exe") } else { dir.join(".venv").join("bin").join("python") }
}

/// uv: $RAEA_UV, the one on the PATH, the one beside raea's executable (the Windows release ships it there), or
/// raea's own, fetched the first time (about 20 MB) into $RAEA_TOOLS_CACHE (else ~/.cache/raea-tools)/uv, as
/// tools/ensure.sh does
pub fn uv() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("RAEA_UV").map(PathBuf::from).filter(|p| p.is_file()) {
        return Ok(p);
    }
    if let Some(p) = on_path("uv").or_else(|| beside_exe("uv")) {
        return Ok(p);
    }
    let dir = raea_io_home::env_path("RAEA_TOOLS_CACHE", ".cache/raea-tools").join("uv");
    let bin = dir.join(exe("uv"));
    if bin.is_file() {
        return Ok(bin);
    }
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let target = match os {
        "macos" => format!("{arch}-apple-darwin"),
        "linux" => format!("{arch}-unknown-linux-gnu"),
        _ => format!("{arch}-pc-windows-msvc"),
    };
    let Some(&(_, sum)) = UV_ARCHIVES.iter().find(|a| a.0 == target) else {
        return Err(format!("raea has no uv for {os} {arch}: install uv (https://docs.astral.sh/uv) and run again"));
    };
    let name = format!("uv-{target}.{}", if os == "windows" { "zip" } else { "tar.gz" });
    raea_io_home::offline(&format!("fetching uv {UV} for a local model's setup"))?;
    eprintln!("raea: fetching uv {UV} ({name}) into {}", dir.display());
    // unpacked beside it first and moved in whole, so a stopped fetch never leaves a uv that looks installed
    let part = dir.with_file_name(format!("uv.part{}", std::process::id()));
    let url = format!("https://github.com/astral-sh/uv/releases/download/{UV}/{name}");
    let fetched = fetch_archive(&part, &url, &name, sum, os != "windows", &[]);
    settle(&part, &dir, &bin, fetched, "uv")
}

/// The Git for Windows raea fetches where there's none (the app's host, apps/app host/tools.ts WIN_GIT, fetches the same
/// into the same folder, so either finds the other's): dugite-native's portable build with git-lfs, GitHub Desktop's
const WIN_GIT_URL: &str = "https://github.com/desktop/dugite-native/releases/download/v2.56.0-2/dugite-native-v2.56.0-a44aeca-windows-x64.tar.gz";
const WIN_GIT_DIR: &str = "git-v2.56.0-2-windows-x64";
const WIN_GIT_SUM: &str = "f1e17876337d7be143e5d51976da36643d65528125cfbc14d57f171de0fd5f14";

/// git, which a workspace is (raea workspace, project, controller): $RAEA_GIT, else the one on the PATH. On Windows,
/// where Git is rarely installed, then the raea app's (its installer puts Git in its folder, which Settings > Apps
/// keeps as InstallLocation), a Git for Windows that isn't on the PATH, and raea's own in $RAEA_TOOLS_CACHE (else
/// ~/.cache/raea-tools), fetched there the first time (47 MB). Until 0.1.8 raea.exe took git from the PATH only, and on
/// a PC with only the app's every workspace command failed with "git: program not found" (#169).
pub fn git() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("RAEA_GIT").map(PathBuf::from).filter(|p| p.is_file()) {
        return Ok(p);
    }
    if let Some(p) = on_path("git") {
        return Ok(p);
    }
    if !cfg!(windows) {
        let how = if cfg!(target_os = "macos") { "xcode-select --install" } else { "apt install git, or dnf install git" };
        return Err(format!("raea's workspaces are git repos, and git isn't installed: {how}"));
    }
    let cache = raea_io_home::env_path("RAEA_TOOLS_CACHE", ".cache/raea-tools").join(WIN_GIT_DIR);
    let bin = cache.join("cmd").join("git.exe");
    if let Some(p) = windows_gits().into_iter().chain([bin.clone()]).find(|p| p.is_file()) {
        return Ok(p);
    }
    static FETCHING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = FETCHING.lock().unwrap_or_else(|e| e.into_inner());
    if bin.is_file() {
        return Ok(bin);
    }
    raea_io_home::offline("fetching Git for Windows: raea's workspaces are git repos")?;
    eprintln!("raea: a workspace is a git repo, and this PC has no Git; fetching raea's own (Git for Windows, once, 47 MB) into {}", cache.display());
    let part = cache.with_file_name(format!("{WIN_GIT_DIR}.part{}", std::process::id()));
    let fetched = fetch_archive(&part, WIN_GIT_URL, "git.tar.gz", WIN_GIT_SUM, false, &[]);
    // it runs before it's taken, as the app's host checks it
    let runs = |p: &Path| std::process::Command::new(p).arg("--version").output().is_ok_and(|o| o.status.success());
    let fetched = fetched.and_then(|()| if runs(&part.join("cmd").join("git.exe")) { Ok(()) } else { Err("the Git raea fetched doesn't run".into()) });
    settle(&part, &cache, &bin, fetched, "Git for Windows")
}

/// The gits a Windows PC may have off its PATH: the raea app's (its install folder's git\cmd), else in the folder its
/// installer uses by default, and Git for Windows' own for everyone and for the one account.
fn windows_gits() -> Vec<PathBuf> {
    let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\raea";
    let reg = std::process::Command::new("reg").args(["query", key, "/v", "InstallLocation"]).output().ok();
    let app = reg.and_then(|o| String::from_utf8_lossy(&o.stdout).lines().find_map(|l| l.trim().strip_prefix("InstallLocation").map(|v| v.trim().trim_start_matches("REG_EXPAND_SZ").trim_start_matches("REG_SZ").trim().to_string())));
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let mut at: Vec<PathBuf> = app.filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).join("git")).into_iter().collect();
    at.extend(env("LOCALAPPDATA").map(|d| d.join("Programs").join("raea").join("git")));
    at.extend(env("ProgramFiles").map(|d| d.join("Git")));
    at.extend(env("LOCALAPPDATA").map(|d| d.join("Programs").join("Git")));
    at.into_iter().map(|d| d.join("cmd").join("git.exe")).collect()
}

/// A fetch's end: its `part` folder moved in as `dir` when it's whole and nothing got there first, the part removed,
/// and, once the tool is in place, the parts stopped fetches left beside it swept (`NAME.part<pid>`, from a Ctrl+C or
/// a killed process: FFmpeg's held a 114 MB zip, and each run made a new one). A part another process is still
/// writing stays on Windows, where its open file can't be removed; elsewhere that fetch fails and finds the tool in
/// place, which counts as done.
fn settle(part: &Path, dir: &Path, bin: &Path, fetched: Result<(), String>, what: &str) -> Result<PathBuf, String> {
    if fetched.is_ok() && !bin.is_file() {
        let _ = std::fs::rename(part, dir);
    }
    let _ = std::fs::remove_dir_all(part);
    if bin.is_file() {
        let stale = dir.file_name().map(|n| format!("{}.part", n.to_string_lossy())).unwrap_or_default();
        let beside = dir.parent().map(std::fs::read_dir).into_iter().flatten().flatten().flatten();
        for e in beside.filter(|e| !stale.is_empty() && e.file_name().to_string_lossy().starts_with(&stale)) {
            let _ = std::fs::remove_dir_all(e.path());
        }
        return Ok(bin.to_path_buf());
    }
    fetched?;
    Err(format!("couldn't put {what} in {}", dir.display()))
}

/// A release archive `name` from `url` downloaded into `part` with curl (resumed across its tries: a dropped connection
/// costs only what wasn't fetched), checked against `sum` and unpacked there by tar: `members` alone when given, its top
/// folder stripped when `strip`. Windows' own curl.exe and tar.exe do it there (bsdtar reads a zip).
fn fetch_archive(part: &Path, url: &str, name: &str, sum: &str, strip: bool, members: &[&str]) -> Result<(), String> {
    use std::io::IsTerminal;
    std::fs::create_dir_all(part).map_err(|e| format!("{}: {e}", part.display()))?;
    let archive = part.join(name);
    let quiet = if std::io::stderr().is_terminal() { "--progress-bar" } else { "-sS" };
    let got = std::process::Command::new(exe("curl"))
        .args(["-fL", quiet, "-C", "-", "--retry", "5", "--retry-all-errors", "--connect-timeout", "30", "-o"])
        .arg(&archive)
        .arg(url)
        .status();
    if !got.is_ok_and(|s| s.success()) {
        return Err(format!("couldn't download {url} with curl"));
    }
    let bytes = std::fs::read(&archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    if format!("{:x}", Sha256::digest(&bytes)) != sum {
        return Err(format!("{name} doesn't match its SHA-256"));
    }
    let mut tar = std::process::Command::new(exe("tar"));
    tar.arg("-xf").arg(&archive).arg("-C").arg(part);
    if strip {
        tar.arg("--strip-components=1");
    }
    tar.args(members);
    if !tar.status().is_ok_and(|s| s.success()) {
        return Err(format!("couldn't unpack {name}"));
    }
    std::fs::remove_file(&archive).map_err(|e| format!("{}: {e}", archive.display()))
}

/// How a person runs a setup themselves: setup.sh on a Mac or Linux, uv on setup.py on Windows
pub fn setup_how(script: &Path) -> String {
    if cfg!(windows) { format!("uv run --no-project --python 3.11 python -I {}", script.with_file_name("setup.py").display()) } else { script.display().to_string() }
}

/// A model's setup, its progress on stderr (stdout is the caller's answer: a render's APPLIED line, the CLI's), with
/// `var` naming the install folder `dir`: tools/<model>/setup.py by uv beside `script` (its setup.sh), else the
/// setup.sh itself by sh.
pub fn run_setup(script: &Path, var: &str, dir: &Path) -> Result<std::process::ExitStatus, String> {
    let py = script.with_file_name("setup.py");
    let mut cmd = if py.exists() {
        let uv = uv()?;
        let mut c = std::process::Command::new(&uv);
        c.args(["run", "-q", "--no-project", "--python", "3.11", "python", "-I"]).arg(&py).env("UV", &uv);
        c
    } else {
        let mut c = std::process::Command::new("sh");
        c.arg(script);
        c
    };
    cmd.env(var, dir).stdout(std::io::stderr()).status().map_err(|e| format!("{}: {e}", py.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // the release's own copy beside the executable is found; a missing one isn't
    #[test]
    fn a_tool_beside_the_executable_is_found() {
        let root = std::env::temp_dir().join(format!("raea-beside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin").join(exe("ffmpeg")), b"").unwrap();
        let raea = root.join("bin").join(exe("raea"));
        assert_eq!(beside(&raea, "ffmpeg"), Some(root.join("bin").join(exe("ffmpeg"))));
        assert_eq!(beside(&raea, "uv"), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    // the pins tools/release/win-bundle.sh reads: one line each, as its sed expects
    #[test]
    fn the_bundle_reads_these_pins() {
        let src = include_str!("setup.rs");
        for c in ["FFMPEG_URL", "FFMPEG_SUM", "FFMPEG_TOP", "UV"] {
            assert_eq!(src.lines().filter(|l| l.starts_with(&format!("const {c}: &str = \""))).count(), 1, "{c}");
        }
        assert!(FFMPEG_URL.ends_with(&format!("/{FFMPEG_TOP}.zip")) && FFMPEG_TOP.contains(FFMPEG));
        assert!(FFMPEG_SUM.len() == 64 && FFMPEG_SUM.bytes().all(|b| b.is_ascii_hexdigit()), "FFMPEG_SUM is a sha256");
        assert!(src.contains("    (\"x86_64-pc-windows-msvc\", \""));
    }

    // a fetch's end sweeps the parts stopped fetches left beside its tool, and only those
    #[test]
    fn settle_sweeps_stopped_fetches() {
        let root = std::env::temp_dir().join(format!("raea-settle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (dir, bin) = (root.join("ffmpeg"), root.join("ffmpeg").join("bin").join("ffmpeg"));
        let mk = |p: &Path, f: &str| {
            std::fs::create_dir_all(p).unwrap();
            std::fs::write(p.join(f), b"x").unwrap();
        };
        // two stopped fetches (one with its zip), another tool's part, and this run's finished part
        mk(&root.join("ffmpeg.part111"), "ffmpeg-9.0.2-lgpl-win64.zip");
        mk(&root.join("ffmpeg.part222"), "x");
        mk(&root.join("uv.part333"), "x");
        let part = root.join("ffmpeg.part444");
        mk(&part.join("bin"), "ffmpeg");
        assert_eq!(settle(&part, &dir, &bin, Ok(()), "FFmpeg").unwrap(), bin);
        assert!(bin.is_file(), "the part moved in");
        for gone in ["ffmpeg.part111", "ffmpeg.part222", "ffmpeg.part444"] {
            assert!(!root.join(gone).exists(), "{gone} swept");
        }
        assert!(root.join("uv.part333").exists(), "another tool's part kept");
        // a fetch that failed but finds the tool in place (another process put it there) is done, its part removed
        let late = root.join("ffmpeg.part555");
        mk(&late, "half.zip");
        assert_eq!(settle(&late, &dir, &bin, Err("curl failed".into()), "FFmpeg").unwrap(), bin);
        assert!(!late.exists());
        // a failure with no tool says so
        let none = root.join("none");
        let err = settle(&root.join("none.part1"), &none, &none.join("bin").join("x"), Err("curl failed".into()), "x").unwrap_err();
        assert_eq!(err, "curl failed");
        let _ = std::fs::remove_dir_all(&root);
    }
}
