//! (a) The ffmpeg CLI: the baseline every other backend is measured against. Decoding keeps one rawvideo pipe open
//! while requests move forward and restarts it with an accurate seek otherwise; frame times come from ffprobe.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use raea_io_codec::{
    Audio, Backend, EncodeOpts, EncodeStats, Frame, Info, R, VideoDecoder, VideoEncoder, as_of,
};

pub struct Ffmpeg;

/// The ffmpeg or ffprobe to run: the PATH's, else raea's own (raea_io_assets::ffmpeg_tool). Opening a video (`fetch`)
/// fetches raea's own on Windows the first time one needs it; sound, encoding and asking what FFmpeg can do (says)
/// use FFmpeg when it's there and never fetch it (a silent clip's sound is tried on every backend).
fn cli(name: &str, fetch: bool) -> R<Command> {
    raea_io_assets::ffmpeg_tool(name, fetch).map(Command::new)
}

/// A file named as a video: only these fetch FFmpeg when it isn't there (an audio file or an image never does)
fn named_video(path: &Path) -> bool {
    const VIDEO: &[&str] = &["mp4", "mov", "m4v", "mkv", "webm", "avi", "mts", "m2ts", "ts", "3gp", "wmv", "flv", "mpg", "mpeg", "mxf", "ogv"];
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    VIDEO.iter().any(|v| *v == ext)
}

/// Every decoded frame, once: -fps_mode from FFmpeg 5.1 on, -vsync before it (FFmpeg 8 took -vsync out, and the decoder
/// read nothing: #113).
pub fn passthrough() -> [&'static str; 2] {
    static FLAG: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    let flag = FLAG.get_or_init(|| {
        let head = says("-version");
        let version = head.split("version ").nth(1).and_then(|v| {
            let mut parts = v.trim_start_matches('n').split(|c: char| !c.is_ascii_digit());
            Some((parts.next()?.parse::<u32>().ok()?, parts.next()?.parse::<u32>().ok()?))
        });
        if version.is_some_and(|v| v < (5, 1)) { "-vsync" } else { "-fps_mode" }
    });
    [flag, "passthrough"]
}

/// What `ffmpeg -hide_banner OPTION` prints (nothing without ffmpeg).
fn says(option: &str) -> String {
    let out = cli("ffmpeg", false).ok().and_then(|mut c| c.args(["-hide_banner", option]).output().ok()).map(|o| o.stdout).unwrap_or_default();
    String::from_utf8_lossy(&out).into_owned()
}

/// The H.264 encoder this machine's ffmpeg has, as its arguments, for a clip a person plays (in a browser or on a
/// phone): libx264 where ffmpeg was built with it (Homebrew's, Linux distributions'), else the system's own (Windows'
/// Media Foundation, h264_mf, in the LGPL build raea ships there; macOS's VideoToolbox) at 8 Mb/s; None without one.
pub fn h264() -> Option<Vec<&'static str>> {
    static ENCODERS: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let have = ENCODERS.get_or_init(|| says("-encoders"));
    let has = |c: &str| have.split_whitespace().any(|w| w == c);
    let enc = ["libx264", "h264_mf", "h264_videotoolbox"].into_iter().find(|c| has(c))?;
    let rate: &[&'static str] = if enc == "libx264" { &[] } else { &["-b:v", "8M"] };
    Some([&["-c:v", enc][..], rate, &["-pix_fmt", "yuv420p"]].concat())
}

/// The ffmpeg and H.264 encoder `Ffmpeg::encoder` uses, chosen once a process: the first of NVIDIA's h264_nvenc,
/// Windows Media Foundation's h264_mf on the GPU (in the LGPL build raea ships there: Intel Quick Sync's or NVIDIA's
/// MFT, whichever Windows offers) and libx264 that some copy of ffmpeg here lists and encodes two frames with (a listed
/// hardware encoder still fails without its GPU or driver). Every copy is asked (raea_io_assets::tool_copies: the PATH's,
/// raea's own beside raea.exe), so the encoder doesn't hang on which comes first on the PATH. RAEA_FFMPEG_ENCODER names
/// the one encoder to look for. None when no ffmpeg here has a working one.
fn pick() -> Option<&'static (PathBuf, &'static str)> {
    static PICK: std::sync::OnceLock<Option<(PathBuf, &'static str)>> = std::sync::OnceLock::new();
    PICK.get_or_init(|| {
        const ALL: [&str; 3] = ["h264_nvenc", "h264_mf", "libx264"];
        let want: Vec<&'static str> = match std::env::var("RAEA_FFMPEG_ENCODER") {
            Ok(w) => ALL.into_iter().filter(|e| *e == w).collect(),
            Err(_) => ALL.to_vec(),
        };
        let copies: Vec<(PathBuf, String)> = raea_io_assets::tool_copies("ffmpeg")
            .into_iter()
            .map(|t| {
                let out = Command::new(&t).args(["-hide_banner", "-encoders"]).stderr(Stdio::null()).output().map(|o| o.stdout).unwrap_or_default();
                (t, String::from_utf8_lossy(&out).into_owned())
            })
            .collect();
        want.into_iter().find_map(|enc| {
            copies.iter().find(|(t, have)| have.split_whitespace().any(|w| w == enc) && encodes(t, enc)).map(|(t, _)| (t.clone(), enc))
        })
    })
    .as_ref()
}

/// The H.264 encoder `Ffmpeg::encoder` uses (pick), None without one.
pub fn video_encoder() -> Option<&'static str> {
    pick().map(|(_, e)| *e)
}

/// The ffmpeg `Ffmpeg::encoder` runs (pick).
pub fn encoder_ffmpeg() -> Option<&'static Path> {
    pick().map(|(t, _)| t.as_path())
}

/// Whether the encoder `Ffmpeg::encoder` uses runs on the GPU (NVENC, or Media Foundation's hardware encoder).
pub fn hardware_encoder() -> bool {
    matches!(video_encoder(), Some("h264_nvenc" | "h264_mf"))
}

/// Two black 256x256 NV12 frames through `ffmpeg`'s `enc` to nowhere: whether it works on this machine.
fn encodes(ffmpeg: &Path, enc: &str) -> bool {
    let child = Command::new(ffmpeg)
        .args(["-v", "error", "-f", "rawvideo", "-pix_fmt", "nv12", "-s", "256x256", "-framerate", "30", "-i", "-"])
        .args(enc_args(enc, None, 2000))
        .args(["-f", "null", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return false };
    let mut frame = vec![16u8; 256 * 256];
    frame.resize(256 * 256 * 3 / 2, 128);
    let fed = child.stdin.as_mut().map_or(false, |i| i.write_all(&frame).and_then(|_| i.write_all(&frame)).is_ok());
    drop(child.stdin.take());
    child.wait().map_or(false, |st| st.success()) && fed
}

/// `enc`'s arguments: constant quality when `quality` is given (0.85 -> CRF/CQ 13, 0.95 -> 11, 0.6 -> 18; Media
/// Foundation's 0-100 scale), else an average of `kbps` with a 1.5x peak.
fn enc_args(enc: &str, quality: Option<f32>, kbps: u32) -> Vec<String> {
    let mut a: Vec<String> = vec!["-c:v".into(), enc.into()];
    let crf = quality.map(|q| format!("{:.0}", 30.0 - 20.0 * q.clamp(0.0, 1.0)));
    let rate = || vec!["-b:v".into(), format!("{kbps}k"), "-maxrate".into(), format!("{}k", kbps * 3 / 2), "-bufsize".into(), format!("{}k", kbps * 2)];
    match enc {
        "h264_nvenc" => {
            a.extend(["-preset", "p5", "-tune", "hq", "-profile:v", "high", "-rc", "vbr"].map(String::from));
            match crf {
                Some(c) => a.extend(["-cq".into(), c, "-b:v".into(), "0".into()]),
                None => a.extend(rate()),
            }
        }
        "h264_mf" => {
            // High profile; h264_mf parses -profile:v only as a number (100 = High, 77 = Main)
            a.extend(["-hw_encoding", "1", "-profile:v", "100"].map(String::from));
            match quality {
                Some(q) => a.extend(["-rate_control".into(), "quality".into(), "-quality".into(), format!("{:.0}", 100.0 * q.clamp(0.0, 1.0))]),
                None => a.extend(["-b:v".into(), format!("{kbps}k")]),
            }
        }
        _ => {
            a.extend(["-preset", "medium"].map(String::from));
            match crf {
                Some(c) => a.extend(["-crf".into(), c]),
                None => a.extend(rate()),
            }
        }
    }
    a
}

/// The facts, the presentation times, whether the colour matrix is untagged, and the decoder a VP8 or VP9 stream with
/// alpha needs (libvpx: ffmpeg's own VP9 decoder drops the alpha WebM keeps beside the picture, #113).
fn probe(path: &Path) -> R<(Info, Vec<f64>, bool, Option<&'static str>)> {
    let out = cli("ffprobe", named_video(path))?
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,width,height,r_frame_rate,color_space:stream_tags=alpha_mode:stream_side_data=rotation:format=duration",
            "-show_entries",
            "packet=pts_time",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe: {e}"))?;
    let j: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("ffprobe json: {e}"))?;
    let s = &j["streams"][0];
    let (mut w, mut h) = (
        s["width"].as_u64().unwrap_or(0) as u32,
        s["height"].as_u64().unwrap_or(0) as u32,
    );
    let rot = s["side_data_list"]
        .as_array()
        .and_then(|l| l.iter().find_map(|d| d["rotation"].as_i64()))
        .unwrap_or(0);
    if rot.abs() == 90 || rot.abs() == 270 {
        std::mem::swap(&mut w, &mut h);
    }
    let fps = s["r_frame_rate"]
        .as_str()
        .and_then(|r| r.split_once('/'))
        .map(|(n, d)| n.parse::<f64>().unwrap_or(30.0) / d.parse::<f64>().unwrap_or(1.0))
        .unwrap_or(30.0);
    let mut pts: Vec<f64> = j["packets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["pts_time"].as_str().and_then(|t| t.parse().ok()))
        .collect();
    pts.sort_by(f64::total_cmp);
    let duration = j["format"]["duration"]
        .as_str()
        .and_then(|d| d.parse().ok())
        .unwrap_or(0.0);
    let untagged = !matches!(s["color_space"].as_str(), Some(c) if c != "unknown");
    let alpha = match (s["codec_name"].as_str(), s["tags"]["alpha_mode"].as_str()) {
        (Some("vp8"), Some("1")) => Some("libvpx"),
        (Some("vp9"), Some("1")) => Some("libvpx-vp9"),
        _ => None,
    };
    // an ffmpeg built without libvpx decodes the picture alone, as before
    static DECODERS: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let have = DECODERS.get_or_init(|| says("-decoders"));
    let alpha = alpha.filter(|c| have.split_whitespace().any(|w| w == *c));
    Ok((
        Info {
            codec: s["codec_name"].as_str().unwrap_or("?").into(),
            width: w,
            height: h,
            fps,
            duration,
            frames: pts.len(),
        },
        pts,
        untagged,
        alpha,
    ))
}

struct Decoder {
    path: PathBuf,
    info: Info,
    pts: Vec<f64>,
    pipe: Option<(Child, usize)>, // the child, and the index of the next frame it will give
    /// untagged HD (720 lines or more) is read as BT.709, as Apple, browsers and our own decoders read it (swscale
    /// would assume BT.601)
    bt709: bool,
    /// the decoder named before the input (libvpx for VP8/VP9 with alpha), else ffmpeg's choice
    vpx: Option<&'static str>,
}

impl Decoder {
    fn start(&mut self, at: usize) -> R<()> {
        if let Some((mut c, _)) = self.pipe.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        let start = self.pts[at] - self.pts[0];
        let mut cmd = cli("ffmpeg", false)?;
        cmd.args(["-v", "error"]).args(self.vpx.map(|c| ["-c:v", c]).into_iter().flatten()).args(["-ss", &format!("{start:.6}"), "-i"]).arg(&self.path);
        if self.bt709 {
            cmd.args(["-vf", "scale=in_color_matrix=bt709:in_range=tv"]);
        }
        let child = cmd
            .args([
                "-an",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
            ])
            .args(passthrough())
            .arg("-")
            .stdout(Stdio::piped())
            // kept for a read that fails (and out of the run's output: the pipe closed under it is "Broken pipe")
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("ffmpeg: {e}"))?;
        self.pipe = Some((child, at));
        Ok(())
    }
}

impl VideoDecoder for Decoder {
    fn info(&self) -> &Info {
        &self.info
    }
    fn index_at(&mut self, t: f64) -> Option<usize> {
        (!self.pts.is_empty()).then(|| raea_io_codec::as_of(&self.pts, t + self.pts[0]))
    }
    fn frame_at(&mut self, t: f64) -> R<Frame> {
        let want = as_of(&self.pts, t + self.pts[0]);
        match &self.pipe {
            Some((_, next)) if *next <= want && want - *next < 60 => {}
            _ => self.start(want)?,
        }
        let size = (self.info.width * self.info.height * 4) as usize;
        let mut buf = vec![0u8; size];
        loop {
            let (child, next) = self.pipe.as_mut().unwrap();
            if let Err(e) = child.stdout.as_mut().unwrap().read_exact(&mut buf) {
                let mut said = String::new();
                let _ = child.stderr.as_mut().map(|s| s.read_to_string(&mut said));
                return Err(format!("ffmpeg read: {e} {}", said.trim()));
            }
            *next += 1;
            if *next - 1 == want {
                return Ok(Frame {
                    pts: self.pts[want] - self.pts[0],
                    width: self.info.width,
                    height: self.info.height,
                    rgba: buf,
                });
            }
        }
    }
}

struct Encoder {
    child: Child,
    video: PathBuf,
    out: PathBuf,
    frames: usize,
    /// the ffmpeg encoder running (video_encoder)
    enc: &'static str,
    /// whether ffmpeg was opened for NV12 frames (EncodeOpts::nv12) rather than RGBA
    nv12: bool,
    width: usize,
    height: usize,
}

impl Encoder {
    fn write(&mut self, bytes: &[u8]) -> R<()> {
        self.child.stdin.as_mut().unwrap().write_all(bytes).map_err(|e| format!("ffmpeg stdin: {e}"))?;
        self.frames += 1;
        Ok(())
    }
}

impl VideoEncoder for Encoder {
    fn push(&mut self, rgba: &[u8]) -> R<()> {
        if self.nv12 {
            let nv12 = raea_io_codec::encode_convert_timed(|| raea_io_codec::yuv::rgba_to_nv12(rgba, self.width, self.height));
            return self.write(&nv12);
        }
        self.write(rgba)
    }
    fn push_nv12(&mut self, nv12: &[u8], w: usize, h: usize) -> R<()> {
        if self.nv12 {
            return self.write(nv12);
        }
        let (y, u, v) = raea_io_codec::yuv::nv12_to_i420(nv12, w, h);
        let mut rgba = Vec::new();
        raea_io_codec::yuv::i420_to_rgba(&y, &u, &v, w, w / 2, w / 2, w, h, &mut rgba);
        self.write(&rgba)
    }
    fn finish(mut self: Box<Self>, audio: Option<&Audio>) -> R<EncodeStats> {
        drop(self.child.stdin.take());
        let st = self.child.wait().map_err(|e| e.to_string())?;
        if !st.success() {
            return Err(format!("ffmpeg encode exited {st}"));
        }
        let mut audio_codec = String::new();
        if let Some(a) = audio {
            // mux: the video stream as is, the audio columns as AAC
            let raw = self.video.with_extension("f32");
            let mut bytes = Vec::with_capacity(a.len() * a.channels.len() * 4);
            for i in 0..a.len() {
                for ch in &a.channels {
                    bytes.extend_from_slice(&ch[i].to_le_bytes());
                }
            }
            std::fs::write(&raw, bytes).map_err(|e| e.to_string())?;
            let st = cli("ffmpeg", false)?
                .args(["-v", "error", "-y", "-i"])
                .arg(&self.video)
                .args([
                    "-f",
                    "f32le",
                    "-ar",
                    &a.rate.to_string(),
                    "-ac",
                    &a.channels.len().to_string(),
                    "-i",
                ])
                .arg(&raw)
                .args([
                    "-map",
                    "0:v",
                    "-map",
                    "1:a",
                    "-c:v",
                    "copy",
                    "-c:a",
                    "aac",
                    "-b:a",
                    "128k",
                    "-movflags",
                    "+faststart",
                    "-shortest",
                ])
                .arg(&self.out)
                .status()
                .map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(&raw);
            if !st.success() {
                return Err("ffmpeg mux failed".into());
            }
            audio_codec = "aac".into();
        } else {
            std::fs::rename(&self.video, &self.out).map_err(|e| e.to_string())?;
        }
        let _ = std::fs::remove_file(&self.video);
        Ok(EncodeStats {
            frames: self.frames,
            bytes: std::fs::metadata(&self.out).map(|m| m.len()).unwrap_or(0),
            video_codec: format!("h264 ({})", self.enc),
            audio_codec,
        })
    }
}

impl Backend for Ffmpeg {
    fn name(&self) -> &'static str {
        "ffmpeg"
    }
    fn open_video(&self, path: &Path) -> R<Box<dyn VideoDecoder>> {
        let (info, pts, untagged, vpx) = probe(path)?;
        let bt709 = untagged && info.width.min(info.height) >= 720;
        if pts.is_empty() {
            return Err(format!("{}: no video frames", path.display()));
        }
        Ok(Box::new(Decoder {
            path: path.to_path_buf(),
            info,
            pts,
            pipe: None,
            bt709,
            vpx,
        }))
    }
    fn decode_audio(&self, path: &Path) -> R<Audio> {
        let ch = cli("ffprobe", false)?
            .args([
                "-v",
                "error",
                "-select_streams",
                "a:0",
                "-show_entries",
                "stream=channels",
                "-of",
                "csv=p=0",
            ])
            .arg(path)
            .output()
            .map_err(|e| e.to_string())?;
        let channels: usize = String::from_utf8_lossy(&ch.stdout)
            .trim()
            .parse()
            .unwrap_or(1);
        let out = cli("ffmpeg", false)?
            .args(["-v", "error", "-i"])
            .arg(path)
            .args(["-vn", "-f", "f32le", "-ar", "48000", "-"])
            .output()
            .map_err(|e| format!("ffmpeg: {e}"))?;
        let samples: Vec<f32> = out
            .stdout
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        let mut cols = vec![Vec::with_capacity(samples.len() / channels.max(1)); channels.max(1)];
        for (i, s) in samples.into_iter().enumerate() {
            cols[i % channels.max(1)].push(s);
        }
        Ok(Audio {
            rate: 48000,
            channels: cols,
        })
    }
    fn encoder(&self, out: &Path, o: &EncodeOpts) -> R<Box<dyn VideoEncoder>> {
        let (tool, enc) = pick().map(|(t, e)| (t, *e)).ok_or("ffmpeg: no H.264 encoder works here (h264_nvenc, h264_mf or libx264)")?;
        let video = out.with_extension("video.mp4");
        // NV12 as the engine's GPU readback gives it (BT.709 limited range), else straight RGBA
        let input = if o.nv12 { "nv12" } else { "rgba" };
        let hw = enc != "libx264";
        let child = Command::new(tool)
            .args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", input])
            .args(o.nv12.then_some(["-color_range", "tv", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"]).into_iter().flatten())
            .args(["-s", &format!("{}x{}", o.width, o.height), "-framerate", &o.fps.to_string(), "-i", "-"])
            .args(enc_args(enc, o.quality, o.kbps))
            .args(["-pix_fmt", if hw { "nv12" } else { "yuv420p" }])
            .args(["-color_range", "tv", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"])
            .args(["-movflags", "+faststart"])
            .arg(&video)
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("ffmpeg: {e}"))?;
        Ok(Box::new(Encoder {
            child,
            video,
            out: out.to_path_buf(),
            frames: 0,
            enc,
            nv12: o.nv12,
            width: o.width as usize,
            height: o.height as usize,
        }))
    }
}

#[cfg(test)]
mod tests;
