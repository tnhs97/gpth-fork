use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::LazyLock;

use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use exif::{In, Reader, Tag, Value};
use regex::Regex;
use serde_json::Value as Json;
use unicode_normalization::UnicodeNormalization;

pub const EXTRA_SUFFIXES: &[&str] = &[
    "-edited",
    "-effects",
    "-smile",
    "-mix",
    "-edytowane",
    "-bearbeitet",
    "-bewerkt",
    "-編集済み",
    "-modificato",
    "-modifié",
    "-ha editado",
    "-editat",
];

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Accuracy {
    Sidecar,
    Exif,
    Filename,
    ApproximateSidecar,
    CreationTime,
}

pub fn extract(
    path: &Path,
    image: bool,
    guess_from_name: bool,
) -> Option<(Accuracy, DateTime<Local>)> {
    sidecar_date(path, false, "photoTakenTime")
        .map(|date| (Accuracy::Sidecar, date))
        .or_else(|| {
            image
                .then(|| exif_date(path))
                .flatten()
                .map(|date| (Accuracy::Exif, date))
        })
        .or_else(|| {
            guess_from_name
                .then(|| filename_date(path))
                .flatten()
                .map(|date| (Accuracy::Filename, date))
        })
        .or_else(|| {
            sidecar_date(path, true, "photoTakenTime")
                .map(|date| (Accuracy::ApproximateSidecar, date))
        })
        .or_else(|| {
            sidecar_date(path, false, "creationTime")
                .or_else(|| sidecar_date(path, true, "creationTime"))
                .map(|date| (Accuracy::CreationTime, date))
        })
}

fn sidecar_date(path: &Path, approximate: bool, key: &str) -> Option<DateTime<Local>> {
    static BRACKET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\([0-9]+\)\.").unwrap());
    static EXTRA: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"-[A-Za-zÀ-ÖØ-öø-ÿ]+(?:\([0-9]\))?\.[\w]+$").unwrap());
    static DIGIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\([0-9]\)\.").unwrap());

    let name = path.file_name()?.to_str()?;
    let stem = path.file_stem()?.to_str()?;
    let normalized: String = name.nfc().collect();
    let mut variants = Vec::new();
    if approximate {
        if let Some(extra) = EXTRA.find(&normalized) {
            let extension = extra.as_str().rfind('.')? + extra.start();
            variants.push(format!(
                "{}{}",
                &normalized[..extra.start()],
                &normalized[extension..]
            ));
        }
        variants.push(DIGIT.replace_all(name, ".").into_owned());
    } else {
        variants.push(name.to_owned());
        let mut units = 0;
        variants.push(
            name.chars()
                .take_while(|c| {
                    units += c.len_utf16();
                    units <= 46
                })
                .collect(),
        );
        if let Some(bracket) = BRACKET.find_iter(name).last() {
            let end = bracket.end() - 1;
            variants.push(format!(
                "{}{}{}",
                &name[..bracket.start()],
                &name[end..],
                &name[bracket.start()..end]
            ));
        }
        if let Some((start, extra)) = EXTRA_SUFFIXES
            .iter()
            .find_map(|extra| normalized.rfind(extra).map(|start| (start, extra)))
        {
            variants.push(format!(
                "{}{}",
                &normalized[..start],
                &normalized[start + extra.len()..]
            ));
        }
        variants.push(stem.to_owned());
        if path.extension().is_none() {
            variants.extend([format!("{name}.jpg"), format!("{name}.mp4")]);
        }
    }

    let mut visited = std::collections::HashSet::new();
    for suffix in [".supplemental-metadata.json", ".json"] {
        for variant in &variants {
            if visited.insert((suffix, variant)) {
                let candidate = path.with_file_name(format!("{variant}{suffix}"));
                if let Some(date) = read_sidecar(&candidate, key) {
                    return Some(date);
                }
            }
        }
    }
    None
}

fn read_sidecar(path: &Path, key: &str) -> Option<DateTime<Local>> {
    let data: Json = serde_json::from_reader(BufReader::new(File::open(path).ok()?)).ok()?;
    let value = &data[key]["timestamp"];
    let epoch = value.as_i64().or_else(|| value.as_str()?.parse().ok())?;
    if epoch == 0 {
        return None;
    }
    DateTime::<Utc>::from_timestamp(epoch, 0).map(|date| date.with_timezone(&Local))
}

fn exif_date(path: &Path) -> Option<DateTime<Local>> {
    let file = File::open(path).ok()?;
    // The EXIF reader loads standalone TIFF containers into memory.
    if file.metadata().ok()?.len() > 64 * 1024 * 1024 {
        return None;
    }
    let mut input = BufReader::new(file);
    let data = Reader::new().read_from_container(&mut input).ok()?;
    [Tag::DateTimeOriginal, Tag::DateTimeDigitized, Tag::DateTime]
        .into_iter()
        .find_map(|tag| {
            let Value::Ascii(values) = &data.get_field(tag, In::PRIMARY)?.value else {
                return None;
            };
            let value = std::str::from_utf8(values.first()?).ok()?;
            let normalized = value
                .replace(['-', '/', '.', '\\'], ":")
                .replace(": ", ":0");
            parse_local(normalized.get(..19)?, "%Y:%m:%d %H:%M:%S")
        })
}

fn filename_date(path: &Path) -> Option<DateTime<Local>> {
    static FORMATS: LazyLock<Vec<(Regex, &str)>> = LazyLock::new(|| {
        [
            (r"(?:18|19|20)[0-9]{6}-[0-9]{6}", "%Y%m%d-%H%M%S"),
            (r"(?:18|19|20)[0-9]{6}_[0-9]{6}", "%Y%m%d_%H%M%S"),
            (r"(?:18|19|20)[0-9]{2}(?:-[0-9]{2}){5}", "%Y-%m-%d-%H-%M-%S"),
            (
                r"(?:18|19|20)[0-9]{2}-[0-9]{2}-[0-9]{2}-[0-9]{6}",
                "%Y-%m-%d-%H%M%S",
            ),
            (r"(?:18|19|20)[0-9]{12}", "%Y%m%d%H%M%S"),
            (r"(?:18|19|20)[0-9]{2}(?:_[0-9]{2}){5}", "%Y_%m_%d_%H_%M_%S"),
        ]
        .into_iter()
        .map(|(pattern, format)| (Regex::new(pattern).unwrap(), format))
        .collect()
    });
    let name = path.file_name()?.to_str()?;
    for (pattern, format) in FORMATS.iter() {
        if let Some(date) = pattern
            .find_iter(name)
            .find_map(|m| parse_local(m.as_str(), format))
        {
            return Some(date);
        }
    }

    let epochs = || name.split(|c: char| !c.is_ascii_digit());
    epochs()
        .filter(|s| s.len() == 13)
        .find_map(|s| {
            let millis = s.parse().ok()?;
            (946_684_800_000..=2_524_608_000_000)
                .contains(&millis)
                .then(|| DateTime::from_timestamp_millis(millis))
                .flatten()
        })
        .or_else(|| {
            epochs().filter(|s| s.len() == 10).find_map(|s| {
                let seconds = s.parse().ok()?;
                (946_684_800..=2_524_608_000)
                    .contains(&seconds)
                    .then(|| DateTime::from_timestamp(seconds, 0))
                    .flatten()
            })
        })
        .map(|date| date.with_timezone(&Local))
}

fn parse_local(value: &str, format: &str) -> Option<DateTime<Local>> {
    let naive = NaiveDateTime::parse_from_str(value, format).ok()?;
    Local.from_local_datetime(&naive).earliest()
}
