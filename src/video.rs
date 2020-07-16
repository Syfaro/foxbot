#[derive(Debug)]
pub struct FfmpegError;

impl std::fmt::Display for FfmpegError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FfmpegError")
    }
}

impl std::error::Error for FfmpegError {}

pub fn process_video(file: &std::path::Path) -> anyhow::Result<String> {
    let id = crate::generate_id();

    let path = format!("videos/{}.mp4", id);

    let output = std::process::Command::new("ffmpeg")
        .arg("-i")
        .arg(file.as_os_str())
        .arg("-acodec")
        .arg("libmp3lame")
        .arg("-c:v")
        .arg("libx264")
        .arg("-crf")
        .arg("26")
        .arg("-fs")
        .arg("50M")
        .arg(&path)
        .output()
        .map_err(|_| {
            std::fs::remove_file(&path).unwrap();
            FfmpegError
        })?;

    tracing::trace!("finished ffmpeg run {:?}", output);

    Ok(path)
}
