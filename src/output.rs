use std::collections::{BTreeMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local};
use clap::ValueEnum;
use tempfile::NamedTempFile;

use crate::media::{Media, MediaFile};

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum Albums {
    /// Create album shortcuts, with real files for album-only media
    #[default]
    Shortcut,
    /// Write a separate file in each album
    DuplicateCopy,
    /// Export all media and write album membership to albums-info.json
    Json,
    /// Export only media from year folders
    Nothing,
}

pub struct Options {
    pub copy: bool,
    pub divide_to_dates: bool,
    pub flat: bool,
    pub split_media: bool,
    pub albums: Albums,
}

pub fn write_media(
    media: Vec<Media>,
    root: &Path,
    input: &Path,
    options: &Options,
) -> Result<usize> {
    let mut album_info = BTreeMap::new();
    let mut used_names = HashSet::new();
    let mut written = 0;
    for Media { files, date } in media {
        let mut main: Option<PathBuf> = None;
        let json = matches!(options.albums, Albums::Json);
        for (album, file) in &files {
            if album.is_some() && matches!(options.albums, Albums::Nothing) {
                continue;
            }
            let folder = folder(
                root,
                if json { None } else { album.as_deref() },
                file,
                date,
                options,
            )?;
            ensure_directory(root, input, &folder)?;

            if matches!(options.albums, Albums::Shortcut)
                && album.is_some()
                && let Some(target) = &main
            {
                match create_shortcut(&folder, target) {
                    Ok(()) => {
                        written += 1;
                        continue;
                    }
                    Err(error) => eprintln!(
                        "Could not create shortcut in {}: {error:#}. Exporting the album file instead.",
                        folder.display()
                    ),
                }
            }

            let mut name = file.path.file_name().unwrap().to_os_string();
            if let Some(extension) = file.inferred_extension {
                name.push(".");
                name.push(extension);
            }
            let target = folder.join(name);
            let target = transfer(&file.path, target, options.copy, &used_names)?;
            if let Err(error) = set_date(
                &target,
                date.map(SystemTime::from).unwrap_or_else(SystemTime::now),
                true,
            ) {
                eprintln!("Could not set the date on {}: {error}", target.display());
            }
            written += 1;
            if json {
                let name = target.file_name().unwrap().to_os_string();
                album_info.insert(
                    name.to_string_lossy().into_owned(),
                    files
                        .keys()
                        .flatten()
                        .map(|album| album.to_string_lossy().into_owned())
                        .collect::<Vec<_>>(),
                );
                // The manifest uses bare filenames across all output subfolders.
                used_names.insert(name);
                break;
            }
            if album.is_none() {
                main = Some(target);
            }
        }
    }
    if matches!(options.albums, Albums::Json) {
        let mut staged = NamedTempFile::new_in(root)?;
        serde_json::to_writer(&mut staged, &album_info)?;
        let manifest = publish(staged, root.join("albums-info.json"), &HashSet::new())?;
        println!("Album information written to {}", manifest.display());
    }
    Ok(written)
}

fn folder(
    root: &Path,
    album: Option<&OsStr>,
    file: &MediaFile,
    date: Option<DateTime<Local>>,
    options: &Options,
) -> Result<PathBuf> {
    let directory = if let Some(album) = album {
        let trimmed = album
            .to_str()
            .map(str::trim)
            .map(OsStr::new)
            .unwrap_or(album);
        let mut components = Path::new(trimmed).components();
        if !matches!(
            (components.next(), components.next()),
            (Some(Component::Normal(_)), None)
        ) {
            bail!("Invalid album directory name {album:?}");
        }
        trimmed
    } else if options.split_media && file.video {
        OsStr::new("ALL_VIDEOS")
    } else {
        OsStr::new("ALL_PHOTOS")
    };
    let mut path = root.join(directory);
    if !options.flat {
        match date {
            None => path.push("date-unknown"),
            Some(date) if options.divide_to_dates => path.push(date.format("%Y/%m").to_string()),
            Some(_) => {}
        }
    }
    Ok(path)
}

fn ensure_directory(root: &Path, input: &Path, folder: &Path) -> Result<()> {
    let mut dir = root.to_path_buf();
    for component in folder.strip_prefix(root)?.components() {
        if !matches!(component, Component::Normal(_)) {
            bail!("Invalid output path {}", folder.display());
        }
        dir.push(component);
        if dir == input {
            bail!("Output folder would overlap input at {}", dir.display());
        }
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let meta = fs::symlink_metadata(&dir)?;
                if !meta.is_dir() || is_link(&meta) {
                    bail!("Expected a regular directory at {}", dir.display());
                }
            }
            Err(error) => return Err(error).with_context(|| format!("Creating {}", dir.display())),
        }
    }
    Ok(())
}

pub fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    metadata.file_type().is_symlink()
}

fn next_name(path: &Path) -> PathBuf {
    let mut name = path.file_stem().unwrap().to_os_string();
    name.push("(1)");
    if let Some(extension) = path.extension() {
        name.push(".");
        name.push(extension);
    }
    path.with_file_name(name)
}

fn publish(
    mut staged: NamedTempFile,
    mut target: PathBuf,
    used: &HashSet<OsString>,
) -> Result<PathBuf> {
    loop {
        if used.contains(target.file_name().unwrap()) {
            target = next_name(&target);
            continue;
        }
        match staged.persist_noclobber(&target) {
            Ok(_) => return Ok(target),
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                staged = error.file;
                target = next_name(&target);
            }
            Err(error) => {
                return Err(error.error).with_context(|| format!("Writing {}", target.display()));
            }
        }
    }
}

fn transfer(
    source: &Path,
    mut target: PathBuf,
    copy: bool,
    used: &HashSet<OsString>,
) -> Result<PathBuf> {
    if !copy {
        // Linking first avoids replacing an existing file and preserves the source
        // until a destination exists. Filesystems without hard links use a staged copy.
        loop {
            if used.contains(target.file_name().unwrap()) {
                target = next_name(&target);
                continue;
            }
            match fs::hard_link(source, &target) {
                Ok(()) => {
                    remove_source(source, &target)?;
                    return Ok(target);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    target = next_name(&target)
                }
                Err(_) => break,
            }
        }
    }
    let staged = NamedTempFile::new_in(target.parent().unwrap())?;
    fs::copy(source, staged.path()).with_context(|| format!("Copying {}", source.display()))?;
    let target = publish(staged, target, used)?;
    if !copy {
        remove_source(source, &target)?;
    }
    Ok(target)
}

fn remove_source(source: &Path, target: &Path) -> Result<()> {
    fs::remove_file(source).with_context(|| {
        format!(
            "Exported to {}, but could not remove {}",
            target.display(),
            source.display()
        )
    })
}

pub fn set_date(path: &Path, time: SystemTime, exported: bool) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::fs::FileTimes;
        use std::os::windows::fs::{FileTimesExt, OpenOptionsExt};
        use std::time::UNIX_EPOCH;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
        let time = if exported { time.max(UNIX_EPOCH) } else { time };
        let mut times = FileTimes::new().set_modified(time);
        if exported {
            times = times.set_created(time);
        }
        File::options()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .open(path)?
            .set_times(times)
    }
    #[cfg(not(windows))]
    {
        let _ = exported;
        File::open(path)?.set_modified(time)
    }
}

#[cfg(unix)]
fn create_shortcut(folder: &Path, target: &Path) -> Result<()> {
    let mut link = folder.join(target.file_name().unwrap());
    let root = folder
        .ancestors()
        .find(|ancestor| target.starts_with(ancestor))
        .unwrap();
    let mut relative = PathBuf::new();
    for _ in folder.strip_prefix(root)?.components() {
        relative.push("..");
    }
    relative.push(target.strip_prefix(root)?);
    loop {
        match std::os::unix::fs::symlink(&relative, &link) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => link = next_name(&link),
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(windows)]
fn create_shortcut(folder: &Path, target: &Path) -> Result<()> {
    use tempfile::Builder;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize, IPersistFile,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::core::{HSTRING, Interface};

    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            // This guard is created only after a successful COM initialization.
            unsafe { CoUninitialize() }
        }
    }
    let mut name = target.file_name().unwrap().to_os_string();
    name.push(".lnk");
    let staged = Builder::new().suffix(".lnk").tempfile_in(folder)?;
    let target = HSTRING::from(target.as_os_str());
    let temporary_link = HSTRING::from(staged.path().as_os_str());
    // COM objects stay on this thread and are dropped before the apartment guard.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let _apartment = Apartment;
        let shortcut: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        shortcut.SetPath(&target)?;
        shortcut.SetRelativePath(&temporary_link, 0)?;
        shortcut
            .cast::<IPersistFile>()?
            .Save(&temporary_link, true)?;
    }
    publish(staged, folder.join(name), &HashSet::new())?;
    Ok(())
}
