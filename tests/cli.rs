use std::fs::{self, File};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{Local, TimeZone};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

struct Takeout {
    _root: TempDir,
    input: PathBuf,
    output: PathBuf,
    year: PathBuf,
    album: PathBuf,
}

impl Takeout {
    fn new() -> Self {
        let root = tempdir().unwrap();
        let input = root.path().join("extracted files");
        let output = root.path().join("output");
        let year = input.join("Takeout/Google Photos/Photos from 2020");
        let album = year.with_file_name("旅の写真");
        fs::create_dir_all(&year).unwrap();
        fs::create_dir_all(&album).unwrap();
        Self {
            _root: root,
            input,
            output,
            year,
            album,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_gpth"));
        command
            .arg("-i")
            .arg(&self.input)
            .arg("-o")
            .arg(&self.output);
        command
    }
}

fn success(result: Output) {
    assert!(
        result.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn modified(path: &Path) -> SystemTime {
    fs::metadata(path).unwrap().modified().unwrap()
}

#[test]
fn organizes_sidecars_albums_and_extensionless_motion_media() {
    let takeout = Takeout::new();
    fs::write(takeout.year.join("image.jpg"), b"shared").unwrap();
    fs::write(takeout.year.join("image(1).jpg"), b"shared").unwrap();
    fs::write(takeout.album.join("image.jpg"), b"shared").unwrap();
    fs::write(takeout.album.join("archive.jpg"), b"only here").unwrap();
    fs::write(takeout.year.join("image-modifie\u{301}.jpg"), b"edited").unwrap();
    fs::write(takeout.year.join("motion"), b"\0\0\0\x18ftypmp42").unwrap();
    let epoch = 1_599_078_832;
    fs::write(
        takeout.album.join("image.jpg.supplemental-metadata.json"),
        format!(
            r#"{{"photoTakenTime":{{"timestamp":"0"}},"creationTime":{{"timestamp":{epoch}}}}}"#
        ),
    )
    .unwrap();
    fs::write(
        takeout.year.join("motion.mp4.json"),
        format!(r#"{{"photoTakenTime":{{"timestamp":"{epoch}"}}}}"#),
    )
    .unwrap();

    success(
        takeout
            .command()
            .args([
                "--copy",
                "--albums=json",
                "--divide-to-dates",
                "--split-media",
                "--skip-extras",
            ])
            .output()
            .unwrap(),
    );

    let date = Local
        .timestamp_opt(epoch, 0)
        .unwrap()
        .format("%Y/%m")
        .to_string();
    let photo = takeout
        .output
        .join("ALL_PHOTOS")
        .join(&date)
        .join("image.jpg");
    let video = takeout
        .output
        .join("ALL_VIDEOS")
        .join(date)
        .join("motion.mp4");
    assert_eq!(
        modified(&photo),
        UNIX_EPOCH + Duration::from_secs(epoch as u64)
    );
    assert_eq!(modified(&video), modified(&photo));
    #[cfg(windows)]
    assert_eq!(
        fs::metadata(photo).unwrap().created().unwrap(),
        modified(&video)
    );
    assert!(
        takeout
            .output
            .join("ALL_PHOTOS/date-unknown/archive.jpg")
            .is_file()
    );
    assert!(takeout.year.join("image.jpg").is_file());
    assert!(takeout.year.join("image(1).jpg").is_file());
    let manifest: Value =
        serde_json::from_slice(&fs::read(takeout.output.join("albums-info.json")).unwrap())
            .unwrap();
    assert_eq!(
        manifest,
        json!({"image.jpg": ["旅の写真"], "archive.jpg": ["旅の写真"], "motion.mp4": []})
    );
}

#[test]
fn creates_portable_album_shortcuts() {
    let takeout = Takeout::new();
    let name = "夏の写真.jpg";
    fs::write(takeout.year.join(name), b"shared").unwrap();
    fs::write(takeout.album.join(name), b"shared").unwrap();
    success(
        takeout
            .command()
            .args(["--copy", "--flat"])
            .output()
            .unwrap(),
    );
    let album = takeout.output.join("旅の写真");
    #[cfg(unix)]
    {
        let relocated = takeout.output.with_file_name("relocated");
        fs::rename(&takeout.output, &relocated).unwrap();
        let link = relocated.join(album.file_name().unwrap()).join(name);
        assert!(fs::read_link(&link).unwrap().is_relative());
        assert_eq!(fs::read(link).unwrap(), b"shared");
    }
    #[cfg(windows)]
    {
        let shortcut = fs::read(album.join(format!("{name}.lnk"))).unwrap();
        let flags = u32::from_le_bytes(shortcut[20..24].try_into().unwrap());
        assert_ne!(flags & 0x8, 0, "shell link should store a relative path");
    }
}

#[test]
fn duplicate_copy_writes_album_files_and_nothing_ignores_them() {
    let takeout = Takeout::new();
    fs::write(takeout.year.join("shared.jpg"), b"shared").unwrap();
    fs::write(takeout.album.join("shared.jpg"), b"shared").unwrap();
    fs::write(takeout.album.join("archive.jpg"), b"archive").unwrap();

    success(
        takeout
            .command()
            .args(["--albums=duplicate-copy", "--flat", "--copy"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        fs::read(takeout.output.join("旅の写真/shared.jpg")).unwrap(),
        b"shared"
    );
    assert!(takeout.output.join("旅の写真/archive.jpg").is_file());
    assert!(takeout.output.join("ALL_PHOTOS/shared.jpg").is_file());

    let output = takeout.output.with_file_name("without albums");
    success(
        Command::new(env!("CARGO_BIN_EXE_gpth"))
            .arg("-i")
            .arg(&takeout.input)
            .arg("-o")
            .arg(&output)
            .args(["--albums=nothing", "--flat"])
            .output()
            .unwrap(),
    );
    assert!(output.join("ALL_PHOTOS/shared.jpg").is_file());
    assert!(!output.join("旅の写真").exists());
    assert!(!takeout.year.join("shared.jpg").exists());
    assert!(takeout.album.join("archive.jpg").is_file());
}

#[test]
fn repairs_dates_from_exif_names_and_legacy_sidecars() {
    let root = tempdir().unwrap();
    let dir = root.path();
    let exif = dir.join("exif.tiff");
    // Little-endian TIFF with one DateTime entry followed by its ASCII value.
    let header = [
        b'I', b'I', 42, 0, 8, 0, 0, 0, // Byte order, signature, IFD offset
        1, 0, // One IFD entry
        0x32, 0x01, 2, 0, 20, 0, 0, 0, 26, 0, 0,
        0, // DateTime tag, type, length, value offset
        0, 0, 0, 0, // No further IFDs
    ];
    fs::write(
        &exif,
        [header.as_slice(), b"2022:12:16 16:06:47\0"].concat(),
    )
    .unwrap();
    let guessed = dir.join("Screenshot_2019-04-16-11-19-37-232.jpg");
    let millis = dir.join("IMG_1613298531209.jpg");
    let numbered = dir.join("img_(87).(vacation).lol(87).jpg");
    let edited = dir.join("simple-edited(1).jpg");
    let broken = dir.join("broken.jpg");
    for file in [&guessed, &millis, &numbered, &edited] {
        fs::write(file, b"example").unwrap();
    }
    fs::write(&broken, b"\xff\xd8\xff\xe1\0\x02").unwrap();
    for name in ["img_(87).(vacation).lol.jpg(87).json", "simple.jpg.json"] {
        fs::write(
            dir.join(name),
            br#"{"photoTakenTime":{"timestamp":"1680289442"}}"#,
        )
        .unwrap();
    }

    let guess_original = modified(&guessed);
    let broken_original = modified(&broken);
    let fix = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_gpth"));
        command.arg("--fix").arg(dir);
        command
    };
    success(fix().arg("--no-guess-from-name").output().unwrap());
    assert_eq!(modified(&guessed), guess_original);
    assert_eq!(modified(&broken), broken_original);
    for path in [&numbered, &edited] {
        assert_eq!(
            modified(path),
            UNIX_EPOCH + Duration::from_secs(1_680_289_442)
        );
    }
    assert_eq!(
        modified(&exif),
        SystemTime::from(Local.with_ymd_and_hms(2022, 12, 16, 16, 6, 47).unwrap())
    );

    success(fix().output().unwrap());
    assert_eq!(
        modified(&guessed),
        SystemTime::from(Local.with_ymd_and_hms(2019, 4, 16, 11, 19, 37).unwrap())
    );
    assert_eq!(
        modified(&millis),
        UNIX_EPOCH + Duration::from_millis(1_613_298_531_209)
    );
}

#[test]
fn distinct_large_files_with_the_same_size_survive_move() {
    let takeout = Takeout::new();
    for (name, byte) in [("one.mp4", 1), ("two.mp4", 2)] {
        let mut source = File::create(takeout.year.join(name)).unwrap();
        source.seek(SeekFrom::Start(64 * 1024 * 1024)).unwrap();
        source.write_all(&[byte]).unwrap();
    }
    success(takeout.command().arg("--flat").output().unwrap());
    for name in ["one.mp4", "two.mp4"] {
        assert_eq!(
            fs::metadata(takeout.output.join("ALL_PHOTOS").join(name))
                .unwrap()
                .len(),
            64 * 1024 * 1024 + 1
        );
        assert!(!takeout.year.join(name).exists());
    }
}

#[test]
fn clearing_output_preserves_nested_input() {
    let root = tempdir().unwrap();
    let output = root.path();
    let input = output.join("keep/input");
    let year = input.join("Photos from 2020");
    fs::create_dir_all(&year).unwrap();
    fs::write(year.join("sample.jpg"), b"takeout").unwrap();
    fs::write(output.join("previous.jpg"), b"old output").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_gpth"))
        .arg("-i")
        .arg(&input)
        .arg("-o")
        .arg(output)
        .args(["--copy", "--flat"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"1\n").unwrap();
    success(child.wait_with_output().unwrap());
    assert_eq!(fs::read(year.join("sample.jpg")).unwrap(), b"takeout");
    assert_eq!(
        fs::read(output.join("ALL_PHOTOS/sample.jpg")).unwrap(),
        b"takeout"
    );
    assert!(!output.join("previous.jpg").exists());
}

#[test]
fn moving_into_existing_output_uses_a_free_name() {
    let takeout = Takeout::new();
    let photos = takeout.output.join("ALL_PHOTOS");
    fs::create_dir_all(&photos).unwrap();
    fs::write(photos.join("sample.jpg"), b"previous export").unwrap();
    fs::write(takeout.year.join("sample.jpg"), b"new export").unwrap();
    let mut child = takeout
        .command()
        .arg("--flat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"2\n").unwrap();
    success(child.wait_with_output().unwrap());
    assert_eq!(
        fs::read(photos.join("sample.jpg")).unwrap(),
        b"previous export"
    );
    assert_eq!(
        fs::read(photos.join("sample(1).jpg")).unwrap(),
        b"new export"
    );
    assert!(!takeout.year.join("sample.jpg").exists());
}

#[test]
fn preserves_capture_dates_from_discarded_duplicates() {
    let epoch = 1_577_836_800;
    // Exercise both retaining the first file and replacing it with a shorter name.
    for (retained, donor, upload_date) in [
        ("a.jpg", "longer.jpg", false),
        ("z.jpg", "a-long.jpg", false),
        ("a.jpg", "longer.jpg", true),
        ("z.jpg", "a-long.jpg", true),
    ] {
        let takeout = Takeout::new();
        for name in [retained, donor] {
            fs::write(takeout.year.join(name), b"identical photo").unwrap();
        }
        fs::write(
            takeout.year.join(format!("{donor}.json")),
            format!(r#"{{"photoTakenTime":{{"timestamp":"{epoch}"}}}}"#),
        )
        .unwrap();
        // The shorter copy has only an upload date, which must not win.
        if upload_date {
            fs::write(
                takeout.year.join(format!("{retained}.json")),
                br#"{"creationTime":{"timestamp":"1704067200"}}"#,
            )
            .unwrap();
        }
        success(
            takeout
                .command()
                .args(["--copy", "--divide-to-dates"])
                .output()
                .unwrap(),
        );
        let directory = Local
            .timestamp_opt(epoch, 0)
            .unwrap()
            .format("%Y/%m")
            .to_string();
        let exported = takeout.output.join("ALL_PHOTOS").join(directory);
        assert_eq!(fs::read_dir(&exported).unwrap().count(), 1);
        assert_eq!(
            modified(&exported.join(retained)),
            UNIX_EPOCH + Duration::from_secs(epoch as u64)
        );
    }
}

#[test]
fn creation_time_is_a_fallback_after_filename_capture_time() {
    let root = tempdir().unwrap();
    let image = root.path().join("IMG_20200101_120000.jpg");
    fs::write(&image, b"photo").unwrap();
    fs::write(
        image.with_file_name("IMG_20200101_120000.jpg.json"),
        br#"{"photoTakenTime":{"timestamp":"0"},"creationTime":{"timestamp":1704067200}}"#,
    )
    .unwrap();
    let fix = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_gpth"));
        command.arg("--fix").arg(root.path());
        command
    };
    success(fix().arg("--no-guess-from-name").output().unwrap());
    assert_eq!(
        modified(&image),
        UNIX_EPOCH + Duration::from_secs(1_704_067_200)
    );
    success(fix().output().unwrap());
    assert_eq!(
        modified(&image),
        SystemTime::from(Local.with_ymd_and_hms(2020, 1, 1, 12, 0, 0).unwrap())
    );
}

#[test]
fn original_exif_date_beats_modification_and_upload_dates() {
    let root = tempdir().unwrap();
    let image = root.path().join("photo.tiff");
    // IFD0 holds DateTime and a pointer to an Exif IFD holding DateTimeOriginal.
    let mut tiff = b"II".to_vec();
    tiff.extend(42_u16.to_le_bytes());
    tiff.extend(8_u32.to_le_bytes());
    tiff.extend(2_u16.to_le_bytes());
    for (tag, kind, count, offset) in [(0x132_u16, 2_u16, 20_u32, 38_u32), (0x8769, 4, 1, 58)] {
        tiff.extend(tag.to_le_bytes());
        tiff.extend(kind.to_le_bytes());
        tiff.extend(count.to_le_bytes());
        tiff.extend(offset.to_le_bytes());
    }
    tiff.extend(0_u32.to_le_bytes());
    tiff.extend(b"2024:01:01 00:00:00\0");
    tiff.extend(1_u16.to_le_bytes());
    tiff.extend(0x9003_u16.to_le_bytes());
    tiff.extend(2_u16.to_le_bytes());
    tiff.extend(20_u32.to_le_bytes());
    tiff.extend(76_u32.to_le_bytes());
    tiff.extend(0_u32.to_le_bytes());
    tiff.extend(b"2020:01:01 12:00:00\0");
    fs::write(&image, tiff).unwrap();
    for with_upload_date in [false, true] {
        if with_upload_date {
            fs::write(
                root.path().join("photo.tiff.supplemental-metadata.json"),
                br#"{"creationTime":{"timestamp":"1704067200"}}"#,
            )
            .unwrap();
        }
        success(
            Command::new(env!("CARGO_BIN_EXE_gpth"))
                .arg("--fix")
                .arg(root.path())
                .output()
                .unwrap(),
        );
        assert_eq!(
            modified(&image),
            SystemTime::from(Local.with_ymd_and_hms(2020, 1, 1, 12, 0, 0).unwrap())
        );
    }
}

#[test]
fn skips_numbered_extras_before_deduplicating_originals() {
    let takeout = Takeout::new();
    let original = "photo-original-with-long-name.jpg";
    fs::write(takeout.year.join(original), b"same photo").unwrap();
    for extra in [
        "a-edited(1).jpg",
        "a-edited(12).jpg",
        "a-edited(1)(2).jpg",
        "a-編集済み(12).jpg",
    ] {
        fs::write(takeout.year.join(extra), b"same photo").unwrap();
    }
    fs::write(takeout.year.join("vacation(12).jpg"), b"another photo").unwrap();
    success(
        takeout
            .command()
            .args(["--copy", "--flat", "--skip-extras"])
            .output()
            .unwrap(),
    );
    let photos = takeout.output.join("ALL_PHOTOS");
    assert_eq!(fs::read_dir(&photos).unwrap().count(), 2);
    assert_eq!(fs::read(photos.join(original)).unwrap(), b"same photo");
    assert!(photos.join("vacation(12).jpg").is_file());
}
