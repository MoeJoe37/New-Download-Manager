//! Local media processing. FFmpeg reads only completed local tracks and writes to a
//! staging file. Inspect the actual streams before committing the output.
use crate::{engine::Job, ytdlp};
use serde::Deserialize;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Duration;

pub(crate) struct Stage {
    dir: PathBuf,
    output: PathBuf,
}

impl Stage {
    pub fn new(destination: &Path) -> Result<Self, String> {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        loop {
            let serial = SERIAL.fetch_add(1, Relaxed);
            let dir = parent.join(format!(".ndm-media-{}-{serial}", std::process::id()));
            match fs::create_dir(&dir) {
                Ok(()) => {
                    let mut output = dir.join("output");
                    if let Some(ext) = destination.extension() {
                        output.set_extension(ext);
                    }
                    return Ok(Self { dir, output });
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err.to_string()),
            }
        }
    }
    pub fn path(&self) -> &Path {
        &self.output
    }
    pub fn finish(self, dest: &Path) -> Result<(), String> {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.output)
            .map_err(|e| e.to_string())?;
        if file.metadata().map_err(|e| e.to_string())?.len() == 0 {
            return Err("Media output is empty".into());
        }
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        fs::rename(&self.output, dest).map_err(|e| e.to_string())
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

pub(crate) fn toolchain() -> Option<PathBuf> {
    let ffmpeg = ytdlp::ffmpeg()?;
    let ffprobe = ytdlp::beside_or_path("ffprobe");
    ytdlp::command(&ffprobe)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
        .then_some(ffmpeg)
}

#[derive(Deserialize)]
struct Track {
    codec_type: String,
    #[serde(default)]
    codec_name: String,
    start_time: Option<String>,
    duration: Option<String>,
    nb_read_packets: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
}
#[derive(Deserialize)]
struct Format {
    format_name: String,
    duration: Option<String>,
}
#[derive(Deserialize)]
struct Inspection {
    streams: Vec<Track>,
    format: Format,
}
fn seconds(value: &Option<String>) -> Option<f64> {
    value
        .as_ref()?
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
}

fn inspect(job: &Job, path: &Path, packets: bool) -> Result<Inspection, String> {
    let ffprobe = ytdlp::beside_or_path("ffprobe");
    let mut cmd = ytdlp::command(&ffprobe);
    cmd.args(["-v", "error", "-protocol_whitelist", "file", "-of", "json",
        "-show_entries", "stream=codec_type,codec_name,start_time,duration,nb_read_packets,width,height:format=format_name,duration"]);
    if packets {
        cmd.arg("-count_packets");
    }
    cmd.arg(path);
    let output = ytdlp::output_unless_stopped(job, cmd)
        .map_err(|e| format!("Media validation needs ffprobe: {e}"))?
        .ok_or("stopped")?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err("ffprobe could not read the media without errors".into());
    }
    serde_json::from_slice(&output.stdout).map_err(|_| "Invalid media inspection output".into())
}

pub(crate) fn validate_audio(job: &Job, output: &Path, ext: &str) -> Result<(), String> {
    let inspected = inspect(job, output, true)?;
    let (container, codec) = if ext == "mp3" { ("mp3", "mp3") } else { ("m4a", "aac") };
    let playable = matches!(inspected.streams.as_slice(), [track]
        if track.codec_type == "audio" && track.codec_name == codec
        && track.nb_read_packets.as_deref().and_then(|n| n.parse::<u64>().ok()).is_some_and(|n| n > 0));
    if !inspected.format.format_name.split(',').any(|name| name == container) || !playable {
        return Err("The converted audio failed media validation".into());
    }
    Ok(())
}

fn mp4_codec(track: &Track) -> bool {
    match track.codec_type.as_str() {
        "video" => matches!(
            track.codec_name.as_str(),
            "h264" | "hevc" | "av1" | "vp9" | "mpeg4" | "mpeg2video" | "mjpeg"
        ),
        "audio" => matches!(
            track.codec_name.as_str(),
            "aac" | "mp3" | "ac3" | "eac3" | "alac" | "opus"
        ),
        _ => true,
    }
}

/// Inspect a bounded prefix of the authorized transport stream before offering conversion.
pub(crate) fn probe_transport(
    agent: &crate::transport::Agent,
    url: &str,
    referrer: &str,
    cookies: &str,
) -> Result<(bool, bool), String> {
    use std::io::Read;
    const SAMPLE: u64 = 188 * 2048;
    let request = crate::transport::request(agent, url, referrer, cookies)
        .set("Range", &format!("bytes=0-{}", SAMPLE - 1));
    let response = crate::transport::call(request)?;
    crate::engine::range::validate_http_response(&response, Some((0, Some(SAMPLE - 1))), None)?;
    let stage = Stage::new(&std::env::temp_dir().join("ndm-probe.ts"))?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(SAMPLE)
        .read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;
    if bytes.first() != Some(&0x47) || bytes.get(188) != Some(&0x47) {
        return Err("The source is not a supported transport stream".into());
    }
    bytes.truncate(bytes.len() - bytes.len() % 188);
    fs::write(stage.path(), bytes).map_err(|err| err.to_string())?;
    let job = Job::new(crate::engine::Info::default());
    let source = inspect(&job, stage.path(), false)?;
    let media: Vec<_> = source
        .streams
        .iter()
        .filter(|t| matches!(t.codec_type.as_str(), "audio" | "video"))
        .collect();
    if media.is_empty() || media.iter().any(|t| t.codec_name.is_empty()) {
        return Err("Could not identify the transport stream codecs".into());
    }
    Ok((
        media.iter().any(|t| !mp4_codec(t)),
        media.iter().any(|t| t.codec_type == "audio"),
    ))
}

pub(crate) fn mux(
    job: &Job,
    ffmpeg: &Path,
    video: &Path,
    audio: Option<&Path>,
    output: &Path,
    ext: &str,
    convert: bool,
) -> Result<(), String> {
    if job.stopping() {
        return Err("stopped".into());
    }
    let first = inspect(job, video, false)?;
    let second = audio.map(|path| inspect(job, path, false)).transpose()?;
    let tracks: Vec<_> = first
        .streams
        .iter()
        .filter(|t| t.codec_type == "video" || audio.is_none() && t.codec_type == "audio")
        .chain(
            second
                .iter()
                .flat_map(|input| input.streams.iter().filter(|t| t.codec_type == "audio")),
        )
        .collect();
    let count = |kind: &str| tracks.iter().filter(|t| t.codec_type == kind).count();
    if tracks.is_empty() || audio.is_some() && (count("video") == 0 || count("audio") == 0) {
        return Err("The source is missing a required media track".into());
    }
    let container = match ext {
        "mp4" | "m4a" => "mp4",
        "ts" => "mpegts",
        "mkv" => "matroska",
        "webm" => "webm",
        _ => return Err("Unsupported output container for media assembly".into()),
    };
    let video_convert = container == "mp4"
        && tracks
            .iter()
            .any(|t| t.codec_type == "video" && !mp4_codec(t));
    let audio_convert = container == "mp4"
        && tracks
            .iter()
            .any(|t| t.codec_type == "audio" && !mp4_codec(t));
    if (video_convert || audio_convert) && !convert {
        return Err("The selected codecs require conversion to create MP4".into());
    }
    let mut cmd = ytdlp::command(ffmpeg);
    cmd.args([
        "-y",
        "-nostdin",
        "-v",
        "error",
        "-xerror",
        "-progress",
        "pipe:1",
        "-nostats",
    ]);
    if audio.is_some() {
        cmd.arg("-copyts");
    }
    cmd.args(["-protocol_whitelist", "file", "-i"]).arg(video);
    if let Some(audio) = audio {
        cmd.args(["-protocol_whitelist", "file", "-i"]).arg(audio);
    }
    cmd.args([
        "-map",
        "0:v?",
        "-map",
        if audio.is_some() { "1:a" } else { "0:a?" },
    ]);
    if matches!(container, "matroska" | "webm") {
        cmd.args(["-map", "0:s?"]);
        if audio.is_some() {
            cmd.args(["-map", "1:s?"]);
        }
    }
    if container == "matroska" {
        cmd.args(["-map", "0:t?"]);
    }
    cmd.args([
        "-map_metadata",
        "0",
        "-map_chapters",
        "0",
        "-c",
        "copy",
        "-avoid_negative_ts",
        "make_zero",
    ]);
    if video_convert {
        cmd.args([
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "18",
            "-threads:v",
            "4",
            "-fps_mode",
            "passthrough",
        ]);
    }
    if audio_convert {
        cmd.args(["-c:a", "aac", "-b:a", "192k", "-threads:a", "2"]);
    }
    cmd.args(["-f", container]).arg(output);
    let duration = [Some(&first), second.as_ref()]
        .into_iter()
        .flatten()
        .filter_map(|input| seconds(&input.format.duration))
        .fold(0.0, f64::max);
    let label = if video_convert || audio_convert {
        "Converting media..."
    } else {
        "Joining video and audio..."
    };
    job.set_post(label, if duration > 0.0 { 0.0 } else { -1.0 });
    let result = (|| {
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let stdout = child.stdout.take().unwrap();
        let position = AtomicU64::new(0);
        let status = std::thread::scope(|scope| {
            scope.spawn(|| {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if let Some(micros) = line
                        .strip_prefix("out_time_us=")
                        .or_else(|| line.strip_prefix("out_time_ms="))
                        .and_then(|t| t.parse::<u64>().ok())
                    {
                        position.fetch_max(micros, Relaxed);
                    }
                }
            });
            loop {
                if job.stopping() {
                    ytdlp::kill_child(&mut child);
                    return Err("stopped".to_string());
                }
                if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                    return if status.success() {
                        Ok(())
                    } else {
                        Err("FFmpeg could not assemble the requested container".into())
                    };
                }
                if duration > 0.0 {
                    job.set_post(
                        label,
                        (position.load(Relaxed) as f64 / (duration * 1_000_000.0)).min(0.99) as f32,
                    );
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        status?;
        job.set_post("Validating media...", -1.0);
        let result = inspect(job, output, true)?;
        let expected_container = if container == "mp4" { "mp4" } else { container };
        if !result
            .format
            .format_name
            .split(',')
            .any(|name| name == expected_container)
        {
            return Err("Media output has the wrong container".into());
        }
        for kind in ["video", "audio"] {
            let actual: Vec<_> = result
                .streams
                .iter()
                .filter(|t| t.codec_type == kind)
                .collect();
            if actual.len() != count(kind)
                || actual.iter().any(|t| {
                    t.codec_name.is_empty()
                        || t.nb_read_packets
                            .as_ref()
                            .and_then(|n| n.parse::<u64>().ok())
                            .unwrap_or(0)
                            == 0
                })
            {
                return Err("Media output is missing valid audio or video packets".into());
            }
            for (original, assembled) in tracks.iter().filter(|t| t.codec_type == kind).zip(actual)
            {
                if kind == "video"
                    && (original.width, original.height) != (assembled.width, assembled.height)
                {
                    return Err("Media output resolution does not match its source".into());
                }
                if let (Some(before), Some(after)) =
                    (seconds(&original.duration), seconds(&assembled.duration))
                {
                    if (before - after).abs() > (before * 0.05).max(0.75) {
                        return Err("Media output duration does not match its source".into());
                    }
                }
            }
        }
        let offset = |tracks: &[&Track]| {
            let time =
                |kind: &str| seconds(&tracks.iter().find(|t| t.codec_type == kind)?.start_time);
            Some(time("audio")? - time("video")?)
        };
        let finished: Vec<_> = result.streams.iter().collect();
        if let (Some(original), Some(actual)) = (offset(&tracks), offset(&finished)) {
            if (original - actual).abs() > 0.15 {
                return Err("Media output could not preserve the audio/video start offset".into());
            }
        }
        Ok(())
    })();
    job.info().post = None;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Info;

    #[test]
    fn failed_staging_keeps_existing_destination() {
        let root = std::env::temp_dir().join(format!("ndm-media-stage-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let dest = root.join("existing.mp4");
        fs::write(&dest, b"previous").unwrap();
        let stage = Stage::new(&dest).unwrap();
        let parent = stage.path().parent().unwrap().to_path_buf();
        fs::write(stage.path(), []).unwrap();
        assert!(stage.finish(&dest).is_err());
        assert_eq!(fs::read(&dest).unwrap(), b"previous");
        assert!(!parent.exists());
        assert!(Stage::new(&dest.join("bad-path.mp4")).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[ignore = "requires FFmpeg and ffprobe"]
    fn actual_media_stream_offers_ts_mp4_and_downloads_selected_quality() {
        use std::io::{BufRead, BufReader, Write};
        let root = std::env::temp_dir().join(format!("ndm-media-qualities-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let ffmpeg = ytdlp::ffmpeg().unwrap();
        let mut files = std::collections::HashMap::new();
        files.insert("/master.m3u8".to_string(), b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100000,RESOLUTION=160x90,CODECS=\"avc1.42c00b,mp4a.40.2\"\nlow.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=300000,RESOLUTION=320x180,CODECS=\"avc1.42c00b,mp4a.40.2\"\nhigh.m3u8\n".to_vec());
        for (label, resolution) in [("low", "160x90"), ("high", "320x180")] {
            let segment = root.join(format!("{label}.ts"));
            assert!(ytdlp::command(&ffmpeg)
                .args([
                    "-y",
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("testsrc2=size={resolution}:rate=10:duration=1"),
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=1",
                    "-c:v",
                    "libx264",
                    "-threads:v",
                    "1",
                    "-c:a",
                    "aac",
                    "-f",
                    "mpegts"
                ])
                .arg(&segment)
                .status()
                .unwrap()
                .success());
            files.insert(format!("/{label}.ts"), fs::read(segment).unwrap());
            files.insert(
                format!("/{label}.m3u8"),
                format!(
                    "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\n{label}.ts\n#EXT-X-ENDLIST\n"
                )
                .into_bytes(),
            );
        }
        let expected_ts = files["/low.ts"].clone();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/master.m3u8", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut socket in listener.incoming().flatten() {
                let mut reader = BufReader::new(&socket);
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                while reader.read_line(&mut String::new()).unwrap_or(0) > 2 {}
                let path = request.split_whitespace().nth(1).unwrap_or("");
                if let Some(bytes) = files.get(path) {
                    let mime = if path.ends_with(".ts") {
                        "video/mp2t"
                    } else {
                        "application/vnd.apple.mpegurl"
                    };
                    let _ = write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len());
                    let _ = socket.write_all(bytes);
                } else {
                    let _ =
                        socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                }
            }
        });
        let agent = crate::transport::client::agent("", "");
        let list =
            serde_json::to_value(crate::stream::formats(&agent, &url, "", "").unwrap()).unwrap();
        assert_eq!(list[0]["resolution"], "160×90");
        assert_eq!(list[1]["resolution"], "320×180");
        assert_eq!(list[0]["outputs"][0], "mp4");
        assert_eq!(list[0]["outputs"][1], "ts");
        for (ext, variant) in [("ts", 0), ("mp4", 1)] {
            let job = Job::new(Info {
                url: url.clone(),
                folder: root.display().to_string(),
                name: format!("selected.{ext}"),
                media_output: ext.into(),
                media_variant: Some(variant),
                ..Default::default()
            });
            assert_eq!(
                crate::stream::run(&job, &agent, &crate::engine::Limiter::default()),
                Ok(true)
            );
            let path = job.info().path();
            assert_eq!(crate::engine::names::sniff(&path), Some(ext));
            let file = inspect(&job, &path, true).unwrap();
            assert_eq!(
                file.streams[0].height,
                Some(if variant == 0 { 90 } else { 180 })
            );
            assert!(file.streams.iter().any(|track| track.codec_type == "audio"));
            if ext == "ts" {
                assert_eq!(fs::read(path).unwrap(), expected_ts);
            }
        }
        let direct = url.replace("master.m3u8", "low.ts");
        assert_eq!(probe_transport(&agent, &direct, "", ""), Ok((false, true)));
        let job = Job::new(Info {
            url: direct,
            folder: root.display().to_string(),
            name: "direct.mp4".into(),
            media_output: "mp4".into(),
            ..Default::default()
        });
        assert_eq!(
            job.run_part(&agent, &crate::engine::Limiter::default()),
            Ok(true)
        );
        let file = inspect(&job, &root.join("direct.mp4"), true).unwrap();
        assert_eq!(file.streams[0].height, Some(90));
        assert!(file.streams.iter().any(|track| track.codec_type == "audio"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[ignore = "requires FFmpeg and ffprobe"]
    fn actual_media_remuxes_ts_preserves_offset_and_muxes_mkv() {
        let root = std::env::temp_dir().join(format!("ndm-media-fixture-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("fixture.ts");
        let ffmpeg = ytdlp::ffmpeg().expect("FFmpeg installed");
        let make = ytdlp::command(&ffmpeg)
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=160x90:rate=12:duration=2",
                "-itsoffset",
                "0.20",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=1.6",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-threads:v",
                "1",
                "-c:a",
                "aac",
                "-f",
                "mpegts",
            ])
            .arg(&source)
            .status()
            .unwrap();
        assert!(make.success());
        let job = Job::new(Info::default());
        let original = inspect(&job, &source, true).unwrap();
        let offset = |file: &Inspection| {
            let time = |kind: &str| {
                seconds(
                    &file
                        .streams
                        .iter()
                        .find(|t| t.codec_type == kind)
                        .unwrap()
                        .start_time,
                )
                .unwrap()
            };
            time("audio") - time("video")
        };
        assert!(offset(&original) > 0.15);
        for ext in ["mp4", "mkv"] {
            let dest = root.join(format!("fixture.{ext}"));
            let stage = Stage::new(&dest).unwrap();
            let start = std::time::Instant::now();
            mux(&job, &ffmpeg, &source, None, stage.path(), ext, false).unwrap();
            stage.finish(&dest).unwrap();
            let result = inspect(&job, &dest, true).unwrap();
            let delta = (offset(&original) - offset(&result)).abs();
            assert!(delta < 0.06, "offset error {delta}");
            assert_eq!(result.streams[0].codec_name, "h264");
            assert_eq!(result.streams[1].codec_name, "aac");
            assert_eq!(
                result.streams[0].nb_read_packets,
                original.streams[0].nb_read_packets
            );
            println!(
                "TS -> {ext}: {} bytes, {:.3}s, offset error {:.3}ms",
                fs::metadata(&dest).unwrap().len(),
                start.elapsed().as_secs_f64(),
                delta * 1000.0
            );
        }
        let legacy_audio = root.join("legacy-audio.ts");
        assert!(ytdlp::command(&ffmpeg)
            .args(["-y", "-v", "error", "-i"])
            .arg(&source)
            .args(["-c:v", "copy", "-c:a", "mp2", "-f", "mpegts"])
            .arg(&legacy_audio)
            .status()
            .unwrap()
            .success());
        let converted = root.join("converted.mp4");
        let stage = Stage::new(&converted).unwrap();
        assert!(mux(
            &job,
            &ffmpeg,
            &legacy_audio,
            None,
            stage.path(),
            "mp4",
            false
        )
        .is_err());
        mux(
            &job,
            &ffmpeg,
            &legacy_audio,
            None,
            stage.path(),
            "mp4",
            true,
        )
        .unwrap();
        stage.finish(&converted).unwrap();
        let result = inspect(&job, &converted, true).unwrap();
        assert_eq!(result.streams[0].codec_name, "h264");
        assert_eq!(result.streams[1].codec_name, "aac");

        let split = |kind: &str, name: &str| {
            let file = root.join(name);
            assert!(ytdlp::command(&ffmpeg)
                .args(["-y", "-v", "error", "-copyts", "-i"])
                .arg(root.join("fixture.mp4"))
                .args(["-map", kind, "-c", "copy"])
                .arg(&file)
                .status()
                .unwrap()
                .success());
            file
        };
        let video = split("0:v", "video.mp4");
        let audio = split("0:a", "audio.m4a");
        let dest = root.join("pair.mkv");
        let stage = Stage::new(&dest).unwrap();
        mux(
            &job,
            &ffmpeg,
            &video,
            Some(&audio),
            stage.path(),
            "mkv",
            false,
        )
        .unwrap();
        stage.finish(&dest).unwrap();
        let result = inspect(&job, &dest, true).unwrap();
        assert_eq!(result.streams.len(), 2);
        assert!((offset(&original) - offset(&result)).abs() < 0.06);
        let damaged = root.join("damaged.ts");
        fs::write(&damaged, b"invalid media").unwrap();
        let previous = fs::read(&dest).unwrap();
        let stage = Stage::new(&dest).unwrap();
        assert!(mux(&job, &ffmpeg, &damaged, None, stage.path(), "mkv", false).is_err());
        assert_eq!(fs::read(&dest).unwrap(), previous);
        assert!(video.exists() && audio.exists());
        job.stop();
        assert_eq!(
            mux(
                &job,
                &ffmpeg,
                &video,
                Some(&audio),
                stage.path(),
                "mkv",
                false
            ),
            Err("stopped".into())
        );
        drop(stage);
        let _ = fs::remove_dir_all(root);
    }
}
