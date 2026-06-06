#![allow(clippy::items_after_test_module)]

use anyhow::Result;
use glob::glob;
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::{fs, time::UNIX_EPOCH};
use tracing::debug;

#[derive(Clone, Copy, Debug, Default)]
pub struct FileAccessOptions {
    pub allow_top_level_directory_symlinks: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FileInfo {
    pub path: String,
    pub size: u64,
    pub last_modified: u64,
}

static IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp"];
static AUDIO_EXTENSIONS: &[&str] = &["m4a", "mp3", "wav", "ogg", "opus", "flac"];
static LYRIC_EXTENSIONS: &[&str] = &["lrc"];

fn is_windows_metadata_file(file_name: &str) -> bool {
    let file_name = file_name.to_ascii_lowercase();
    file_name == "desktop.ini"
        || file_name.starts_with("albumartsmall")
        || file_name.starts_with("albumart_{")
}

fn path_has_hidden_component(path: &Path) -> bool {
    path.components().any(|component| match component {
        Component::Normal(part) => part.to_str().is_some_and(|name| name.starts_with('.')),
        _ => false,
    })
}

fn path_has_hidden_attribute(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        return fs::metadata(path)
            .map(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0)
            .unwrap_or(false);
    }

    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

fn file_is_syncable(path: &Path) -> bool {
    if let Some(file_name) = path.file_name().and_then(OsStr::to_str) {
        if file_name.starts_with('.') {
            return false;
        }

        if is_windows_metadata_file(file_name) {
            return false;
        }
    }

    syncable_content_type(path).is_some()
}

pub fn syncable_content_type(path: &Path) -> Option<&'static str> {
    let ext = path.extension().and_then(OsStr::to_str)?;
    let ext = ext.to_ascii_lowercase();

    if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        return Some(match ext.as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            "png" => "image/png",
            "webp" => "image/webp",
            _ => unreachable!("all image extensions have content types"),
        });
    }

    if AUDIO_EXTENSIONS.contains(&ext.as_str()) {
        return Some(match ext.as_str() {
            "m4a" => "audio/mp4",
            "mp3" => "audio/mpeg",
            "wav" => "audio/wav",
            "ogg" => "audio/ogg",
            "opus" => "audio/opus",
            "flac" => "audio/flac",
            _ => unreachable!("all audio extensions have content types"),
        });
    }

    if LYRIC_EXTENSIONS.contains(&ext.as_str()) {
        return Some("text/plain; charset=utf-8");
    }

    None
}

pub fn resolve_syncable_file_path(dir: &Path, relative_path: &str) -> Result<PathBuf> {
    resolve_syncable_file_path_with_options(dir, relative_path, FileAccessOptions::default())
}

pub fn resolve_syncable_file_path_with_options(
    dir: &Path,
    relative_path: &str,
    options: FileAccessOptions,
) -> Result<PathBuf> {
    debug!("resolving path: {:?}", relative_path);

    if relative_path.is_empty() {
        debug!("rejected: path is empty");
        anyhow::bail!("path cannot be empty");
    }

    if path_has_hidden_component(Path::new(relative_path)) {
        debug!("rejected: path has hidden component: {:?}", relative_path);
        anyhow::bail!("hidden paths are not syncable");
    }

    let mut resolved_path = dir.to_path_buf();
    for component in Path::new(relative_path).components() {
        match component {
            Component::Normal(part) => resolved_path.push(part),
            _ => {
                debug!(
                    "rejected: non-normal path component {:?} in {:?}",
                    component.as_os_str(),
                    relative_path
                );
                anyhow::bail!("path must be a relative child of the shared directory");
            }
        }
    }

    if !resolved_path.is_file() {
        debug!("rejected: not a file on disk: {:?}", resolved_path);
        anyhow::bail!("path does not point to a file");
    }

    if path_has_hidden_attribute(&resolved_path) {
        debug!("rejected: hidden file attribute: {:?}", resolved_path);
        anyhow::bail!("hidden paths are not syncable");
    }

    if !file_is_syncable(&resolved_path) {
        debug!("rejected: not a syncable file type: {:?}", resolved_path);
        anyhow::bail!("path is not syncable");
    }

    let canonical_dir = dir.canonicalize()?;
    let canonical_path = resolved_path.canonicalize()?;
    if !canonical_path.starts_with(&canonical_dir)
        && (!options.allow_top_level_directory_symlinks
            || !via_top_level_directory_symlink(dir, relative_path))
    {
        debug!(
            "rejected: canonical path {:?} escapes shared dir {:?}",
            canonical_path, canonical_dir
        );
        anyhow::bail!("path must stay inside the shared directory");
    }

    if !file_is_syncable(&canonical_path) {
        debug!(
            "rejected: canonical path not syncable: {:?}",
            canonical_path
        );
        anyhow::bail!("path is not syncable");
    }

    debug!("resolved ok: {:?}", canonical_path);
    Ok(canonical_path)
}

fn via_top_level_directory_symlink(dir: &Path, relative_path: &str) -> bool {
    let first_component = Path::new(relative_path).components().next();
    matches!(
        first_component,
        Some(Component::Normal(c)) if {
            let p = dir.join(c);
            p.is_symlink() && p.is_dir()
        }
    )
}

#[cfg(test)]
mod tests {
    use super::{
        file_is_syncable, list_files, list_files_with_options, resolve_syncable_file_path,
        resolve_syncable_file_path_with_options, syncable_content_type, FileAccessOptions,
    };
    use std::fs;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_test_dir(prefix: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("minimoon-sync-{prefix}-{unique}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn excludes_windows_album_art_and_desktop_ini_files() {
        assert!(!file_is_syncable(Path::new("AlbumArtSmall.jpg")));
        assert!(!file_is_syncable(Path::new(
            "AlbumArt_{12345678-1234-1234-1234-1234567890AB}_Large.jpg"
        )));
        assert!(!file_is_syncable(Path::new("desktop.ini")));
    }

    #[test]
    fn keeps_normal_media_files_syncable() {
        assert!(file_is_syncable(Path::new("track01.mp3")));
        assert!(file_is_syncable(Path::new("TRACK01.MP3")));
        assert!(file_is_syncable(Path::new("track01.wav")));
        assert!(file_is_syncable(Path::new("TRACK01.WAV")));
        assert!(file_is_syncable(Path::new("cover.jpg")));
        assert!(file_is_syncable(Path::new("cover.png")));
        assert!(file_is_syncable(Path::new("cover.webp")));
        assert!(file_is_syncable(Path::new("lyrics.lrc")));
    }

    #[test]
    fn maps_wav_files_to_audio_wav_content_type() {
        assert_eq!(
            syncable_content_type(Path::new("track01.wav")),
            Some("audio/wav")
        );
    }

    #[test]
    fn rejects_other_text_files() {
        assert!(!file_is_syncable(Path::new("notes.txt")));
    }

    #[test]
    fn listing_excludes_hidden_files_and_directories() {
        let root = temp_test_dir("hidden-list");
        fs::write(root.join(".hidden-track.mp3"), "hidden").unwrap();
        fs::write(root.join("visible.mp3"), "visible").unwrap();
        let hidden_dir = root.join(".hidden-album");
        fs::create_dir_all(&hidden_dir).unwrap();
        fs::write(hidden_dir.join("track.mp3"), "hidden").unwrap();

        let files = list_files(root.to_string_lossy().into_owned()).unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "visible.mp3");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_absolute_paths() {
        let root = temp_test_dir("absolute");
        let file_path = root.join("track.mp3");
        fs::write(&file_path, "test").unwrap();

        let error =
            resolve_syncable_file_path(&root, file_path.to_string_lossy().as_ref()).unwrap_err();

        assert!(error
            .to_string()
            .contains("relative child of the shared directory"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_current_directory_components() {
        let root = temp_test_dir("current-dir");
        fs::write(root.join("track.mp3"), "test").unwrap();

        let error = resolve_syncable_file_path(&root, "./track.mp3").unwrap_err();

        assert!(error
            .to_string()
            .contains("relative child of the shared directory"));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_that_escape_shared_directory() {
        use std::os::unix::fs::symlink;

        let root = temp_test_dir("symlink-root");
        let outside = temp_test_dir("symlink-outside");
        let outside_file = outside.join("secret.mp3");
        fs::write(&outside_file, "secret").unwrap();
        symlink(&outside_file, root.join("linked.mp3")).unwrap();

        let error = resolve_syncable_file_path(&root, "linked.mp3").unwrap_err();

        assert!(error.to_string().contains("inside the shared directory"));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_top_level_directory_symlinks_by_default() {
        use std::os::unix::fs::symlink;

        let root = temp_test_dir("symlink-dir-root");
        let outside = temp_test_dir("symlink-dir-outside");
        let outside_file = outside.join("track.mp3");
        fs::write(&outside_file, "test").unwrap();
        // Symlink an entire directory into root (like an artist folder)
        symlink(&outside, root.join("Artist")).unwrap();

        let error = resolve_syncable_file_path(&root, "Artist/track.mp3").unwrap_err();

        assert!(error.to_string().contains("inside the shared directory"));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn allows_files_inside_top_level_directory_symlinks_when_enabled() {
        use std::os::unix::fs::symlink;

        let root = temp_test_dir("symlink-dir-root-enabled");
        let outside = temp_test_dir("symlink-dir-outside-enabled");
        let outside_file = outside.join("track.mp3");
        fs::write(&outside_file, "test").unwrap();
        symlink(&outside, root.join("Artist")).unwrap();

        let resolved = resolve_syncable_file_path_with_options(
            &root,
            "Artist/track.mp3",
            FileAccessOptions {
                allow_top_level_directory_symlinks: true,
            },
        )
        .unwrap();

        assert_eq!(resolved, outside_file.canonicalize().unwrap());
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_to_non_syncable_files_inside_shared_directory() {
        use std::os::unix::fs::symlink;

        let root = temp_test_dir("symlink-nonsyncable");
        let target = root.join("secret.txt");
        fs::write(&target, "secret").unwrap();
        symlink(&target, root.join("linked.mp3")).unwrap();

        let error = resolve_syncable_file_path(&root, "linked.mp3").unwrap_err();

        assert!(error.to_string().contains("not syncable"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolves_syncable_relative_file_paths() {
        let root = temp_test_dir("resolve");
        let nested_dir = root.join("Albums");
        fs::create_dir_all(&nested_dir).unwrap();
        let file_path = nested_dir.join("track #1?.opus");
        fs::write(&file_path, "test").unwrap();

        let resolved = resolve_syncable_file_path(&root, "Albums/track #1?.opus").unwrap();

        assert_eq!(resolved, file_path.canonicalize().unwrap());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_parent_directory_traversal() {
        let root = temp_test_dir("traversal");

        let error = resolve_syncable_file_path(&root, "../secret.mp3").unwrap_err();

        assert!(error
            .to_string()
            .contains("relative child of the shared directory"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_non_syncable_files() {
        let root = temp_test_dir("nonsyncable");
        let file_path = root.join("notes.txt");
        fs::write(&file_path, "test").unwrap();

        let error = resolve_syncable_file_path(&root, "notes.txt").unwrap_err();

        assert!(error.to_string().contains("not syncable"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_hidden_relative_file_paths() {
        let root = temp_test_dir("hidden-resolve");
        let hidden_dir = root.join(".hidden-album");
        fs::create_dir_all(&hidden_dir).unwrap();
        fs::write(hidden_dir.join("track.mp3"), "test").unwrap();

        let error = resolve_syncable_file_path(&root, ".hidden-album/track.mp3").unwrap_err();

        assert!(error.to_string().contains("hidden paths"));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn listing_excludes_top_level_directory_symlinks_by_default() {
        use std::os::unix::fs::symlink;

        let root = temp_test_dir("symlink-list-root");
        let outside = temp_test_dir("symlink-list-outside");
        fs::write(outside.join("track.mp3"), "test").unwrap();
        symlink(&outside, root.join("Artist")).unwrap();

        let files = list_files(root.to_string_lossy().into_owned()).unwrap();

        assert!(files.is_empty());
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn listing_includes_top_level_directory_symlinks_when_enabled() {
        use std::os::unix::fs::symlink;

        let root = temp_test_dir("symlink-list-root-enabled");
        let outside = temp_test_dir("symlink-list-outside-enabled");
        fs::write(outside.join("track.mp3"), "test").unwrap();
        symlink(&outside, root.join("Artist")).unwrap();

        let files = list_files_with_options(
            root.to_string_lossy().into_owned(),
            FileAccessOptions {
                allow_top_level_directory_symlinks: true,
            },
        )
        .unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "Artist/track.mp3");
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }
}

fn file_info(dir: &Path, path: &Path) -> Result<FileInfo> {
    let metadata = fs::metadata(path).unwrap();
    let last_modified = metadata.modified()?.duration_since(UNIX_EPOCH)?.as_secs();
    let relative_path = path.strip_prefix(dir)?;
    Ok(FileInfo {
        path: relative_path.to_string_lossy().replace('\\', "/"),
        size: metadata.len(),
        last_modified: last_modified * 1000,
    })
}

pub fn list_files(dir: impl Into<PathBuf>) -> Result<Vec<FileInfo>> {
    list_files_with_options(dir, FileAccessOptions::default())
}

pub fn list_files_with_options(
    dir: impl Into<PathBuf>,
    options: FileAccessOptions,
) -> Result<Vec<FileInfo>> {
    let root_path = dir.into();
    if root_path.as_os_str().is_empty() {
        return Ok(Vec::new());
    }

    let mut file_infos = Vec::<FileInfo>::new();
    let pattern = format!("{}/**/*", root_path.to_string_lossy());

    for entry in glob(&pattern).expect("Failed to read glob pattern") {
        match entry {
            Ok(path) => {
                let relative_path = path.strip_prefix(&root_path)?;
                let relative_path_string = relative_path.to_string_lossy();
                if path.is_file()
                    && !path_has_hidden_component(relative_path)
                    && !path_has_hidden_attribute(path.as_path())
                    && file_is_syncable(path.as_path())
                    && resolve_syncable_file_path_with_options(
                        &root_path,
                        relative_path_string.as_ref(),
                        options,
                    )
                    .is_ok()
                {
                    let info = file_info(&root_path, path.as_path())?;
                    file_infos.push(info);
                }
            }
            Err(e) => eprintln!("{:?}", e),
        }
    }

    Ok(file_infos)
}
