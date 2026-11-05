use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use regex::Regex;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;
use walkdir::WalkDir;

use crate::dates;

pub struct MediaFile {
    pub path: PathBuf,
    pub video: bool,
    pub inferred_extension: Option<&'static str>,
}

pub struct Media {
    pub files: BTreeMap<Option<OsString>, MediaFile>,
    pub date: Option<DateTime<Local>>,
}

pub fn media_file(path: PathBuf) -> Option<MediaFile> {
    let mime = mime_guess::from_path(&path).first();
    let extension = path.extension().and_then(OsStr::to_str).unwrap_or_default();
    let kind = mime.as_ref().map(|mime| mime.type_().as_str());
    let (video, inferred_extension) = if kind == Some("image") {
        (false, None)
    } else if kind == Some("video")
        || ["mts", "m2ts", "3gp", "m4v", "mkv", "webm", "wmv"]
            .iter()
            .any(|ext| extension.eq_ignore_ascii_case(ext))
    {
        (true, None)
    } else if mime.is_none() && !extension.eq_ignore_ascii_case("json") {
        let mut header = Vec::with_capacity(32);
        File::open(&path)
            .ok()?
            .take(32)
            .read_to_end(&mut header)
            .ok()?;
        let (video, detected_extension) = if header.get(4..8) == Some(b"ftyp") {
            (true, "mp4")
        } else if header.starts_with(b"\xff\xd8\xff") {
            (false, "jpg")
        } else if header.starts_with(b"\x89PNG") {
            (false, "png")
        } else {
            let detected = infer::get(&header)?;
            match detected.matcher_type() {
                infer::MatcherType::Image => (false, detected.extension()),
                infer::MatcherType::Video => (true, detected.extension()),
                _ => return None,
            }
        };
        (
            video,
            path.extension().is_none().then_some(detected_extension),
        )
    } else {
        return None;
    };
    Some(MediaFile {
        path,
        video,
        inferred_extension,
    })
}

pub fn walk(
    root: &Path,
    output: Option<&Path>,
) -> impl Iterator<Item = walkdir::Result<walkdir::DirEntry>> {
    WalkDir::new(root)
        .min_depth(1)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(move |entry| Some(entry.path()) != output)
}

fn is_year_dir(name: &OsStr) -> bool {
    static YEAR: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^Photos from (?:18|19|20)[0-9]{2}$").unwrap());
    name.to_str().is_some_and(|name| YEAR.is_match(name))
}

pub fn collect(input: &Path, output: &Path) -> Result<Vec<(Option<OsString>, MediaFile)>> {
    let mut years = Vec::new();
    let mut other_dirs = Vec::new();
    let mut year_parents = HashSet::new();
    for entry in walk(input, Some(output)) {
        let entry = entry?;
        if entry.file_type().is_dir() {
            if is_year_dir(entry.file_name()) {
                let path = entry.into_path();
                year_parents.insert(path.parent().unwrap().to_path_buf());
                years.push(path);
            } else {
                other_dirs.push(entry.into_path());
            }
        }
    }

    let albums = other_dirs.into_iter().filter(|dir| {
        dir.parent()
            .is_some_and(|parent| year_parents.contains(parent))
    });
    let mut media = Vec::new();
    for (album, dir) in years
        .into_iter()
        .map(|dir| (None, dir))
        .chain(albums.map(|dir| (dir.file_name().map(OsStr::to_os_string), dir)))
    {
        let mut entries = fs::read_dir(&dir)?.collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            if entry.file_type()?.is_file()
                && let Some(file) = media_file(entry.path())
            {
                media.push((album.clone(), file));
            }
        }
    }
    Ok(media)
}

pub struct GroupedMedia {
    pub media: Vec<Media>,
    pub duplicates: usize,
    pub extras: usize,
}

pub fn group(
    candidates: Vec<(Option<OsString>, MediaFile)>,
    skip_extras: bool,
    guess_from_name: bool,
) -> Result<GroupedMedia> {
    let mut sizes = HashMap::<u64, usize>::new();
    let mut sized = Vec::with_capacity(candidates.len());
    for (album, file) in candidates {
        let size = fs::metadata(&file.path)
            .with_context(|| format!("Reading {}", file.path.display()))?
            .len();
        *sizes.entry(size).or_default() += 1;
        sized.push((size, album, file));
    }

    let mut indices = HashMap::new();
    let mut groups = Vec::<BTreeMap<_, MediaFile>>::new();
    let mut duplicates = 0;
    for (size, album, file) in sized {
        let hash = if sizes[&size] == 1 {
            None
        } else {
            let mut input = File::open(&file.path)?;
            let mut digest = Sha256::new();
            io::copy(&mut input, &mut digest)
                .with_context(|| format!("Hashing {}", file.path.display()))?;
            Some(digest.finalize())
        };
        let index = *indices.entry((size, hash)).or_insert_with(|| {
            groups.push(BTreeMap::new());
            groups.len() - 1
        });
        let files = &mut groups[index];
        if let Some(existing) = files.get_mut(&album) {
            duplicates += 1;
            if name_length(&file.path) < name_length(&existing.path) {
                *existing = file;
            }
        } else {
            files.insert(album, file);
        }
    }

    let mut media = Vec::new();
    let mut extras = 0;
    for mut files in groups {
        if skip_extras {
            let count = files.len();
            files.retain(|_, file| !is_extra(&file.path));
            extras += count - files.len();
        }
        if !files.is_empty() {
            let date = files
                .values()
                .filter_map(|file| dates::extract(&file.path, !file.video, guess_from_name))
                .min_by_key(|(accuracy, _)| *accuracy)
                .map(|(_, date)| date);
            media.push(Media { files, date });
        }
    }
    Ok(GroupedMedia {
        media,
        duplicates,
        extras,
    })
}

fn name_length(path: &Path) -> usize {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .encode_utf16()
        .count()
}

fn is_extra(path: &Path) -> bool {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let normalized: String = stem.to_lowercase().nfc().collect();
    dates::EXTRA_SUFFIXES
        .iter()
        .any(|suffix| normalized.ends_with(suffix))
}
