mod dates;
mod media;
mod output;

use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{ArgAction, Parser, ValueEnum};

use output::Albums;

#[derive(Parser)]
#[command(
    version,
    about,
    args_override_self = true,
    after_help = "Extract and merge all Takeout archives into one input folder before running."
)]
struct Args {
    /// Input folder containing the extracted Takeout archives
    #[arg(short, long)]
    input: Option<PathBuf>,

    /// Destination folder
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Fix modification times in this folder without organizing media
    #[arg(long)]
    fix: Option<PathBuf>,

    /// Prompt for missing paths, date layout, and album mode
    #[arg(long, overrides_with = "no_interactive")]
    interactive: bool,

    /// Disable automatic interactive mode
    #[arg(long, overrides_with = "interactive")]
    no_interactive: bool,

    /// How to export albums
    #[arg(long, value_enum)]
    albums: Option<Albums>,

    /// Divide output into year/month folders
    #[arg(long, overrides_with = "no_divide_to_dates")]
    divide_to_dates: bool,

    #[arg(long, hide = true, overrides_with = "divide_to_dates")]
    no_divide_to_dates: bool,

    /// Place media directly in the main and album folders
    #[arg(long, overrides_with = "no_flat")]
    flat: bool,

    #[arg(long, hide = true, overrides_with = "flat")]
    no_flat: bool,

    /// Put videos in ALL_VIDEOS and photos in ALL_PHOTOS
    #[arg(long, overrides_with = "no_split_media")]
    split_media: bool,

    #[arg(long, hide = true, overrides_with = "split_media")]
    no_split_media: bool,

    /// Skip edited copies and other variants
    #[arg(long, overrides_with = "no_skip_extras")]
    skip_extras: bool,

    #[arg(long, hide = true, overrides_with = "skip_extras")]
    no_skip_extras: bool,

    /// Try to infer dates from filenames (enabled by default)
    #[arg(long, action = ArgAction::SetTrue, overrides_with = "no_guess_from_name")]
    guess_from_name: bool,

    /// Disable filename-based date guessing
    #[arg(long, overrides_with = "guess_from_name")]
    no_guess_from_name: bool,

    /// Copy files instead of moving them
    #[arg(long, overrides_with = "no_copy")]
    copy: bool,

    #[arg(long, hide = true, overrides_with = "copy")]
    no_copy: bool,
}

fn main() -> ExitCode {
    let auto_interactive = std::env::args_os().len() == 1 && io::stdin().is_terminal();
    let args = Args::parse();
    let code = match run(args, auto_interactive) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("Error: {error:#}");
            1
        }
    };
    if auto_interactive {
        let _ = prompt("Press Enter to close");
    }
    ExitCode::from(code)
}

fn run(mut args: Args, auto_interactive: bool) -> Result<u8> {
    let guess_from_name = !args.no_guess_from_name;
    if let Some(fix) = args.fix {
        if !fix.is_dir() {
            eprintln!("Directory to fix does not exist: {}", fix.display());
            return Ok(11);
        }
        fix_dates(&fix, guess_from_name)?;
        return Ok(0);
    }

    let interactive = args.interactive || (auto_interactive && !args.no_interactive);
    if interactive {
        complete_interactive(&mut args)?;
    }
    let (Some(input), Some(destination)) = (args.input, args.output) else {
        eprintln!("Specify both --input and --output, or run gpth --help.");
        return Ok(10);
    };
    if !input.is_dir() {
        eprintln!("Input folder does not exist: {}", input.display());
        return Ok(11);
    }
    let input = dunce::canonicalize(input)?;
    fs::create_dir_all(&destination)
        .with_context(|| format!("Creating output folder {}", destination.display()))?;
    let destination = dunce::canonicalize(destination)?;
    if input == destination {
        bail!("Input and output must be different directories");
    }

    println!("Searching for media in {}", input.display());
    let candidates = media::collect(&input, &destination)?;
    if candidates.is_empty() {
        eprintln!("No media found. Check for extracted folders named like \"Photos from 2024\".");
        return Ok(13);
    }
    println!("Finding duplicates, dates, and album membership");
    let grouped = media::group(candidates, args.skip_extras, guess_from_name)?;
    if !prepare_output(&destination, &input)? {
        return Ok(0);
    }
    let unknown_dates = grouped
        .media
        .iter()
        .filter(|media| media.date.is_none())
        .count();
    let options = output::Options {
        copy: args.copy,
        divide_to_dates: args.divide_to_dates,
        flat: args.flat,
        split_media: args.split_media,
        albums: args.albums.unwrap_or_default(),
    };
    println!(
        "{} media to {}",
        if args.copy { "Copying" } else { "Moving" },
        destination.display()
    );
    let written = output::write_media(grouped.media, &destination, &input, &options)?;
    println!("Exported {written} files and shortcuts.");
    println!(
        "Skipped {} duplicates and {} extras.",
        grouped.duplicates, grouped.extras
    );
    if unknown_dates > 0 {
        println!("Could not determine a date for {unknown_dates} media items.");
    }
    Ok(0)
}

fn fix_dates(root: &Path, guess_from_name: bool) -> Result<()> {
    let mut fixed = 0;
    let mut unknown = 0;
    for entry in media::walk(root, None) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        if let Some(file) = media::media_file(entry.into_path()) {
            if let Some((_, date)) = dates::extract(&file.path, !file.video, guess_from_name) {
                output::set_date(&file.path, date.into(), false)
                    .with_context(|| format!("Setting the date on {}", file.path.display()))?;
                fixed += 1;
            } else {
                unknown += 1;
            }
        }
    }
    println!("Set dates on {fixed} files. No date found for {unknown} files.");
    Ok(())
}

fn complete_interactive(args: &mut Args) -> Result<()> {
    println!("Google Photos Takeout Helper {}", env!("CARGO_PKG_VERSION"));
    if args.input.is_none() {
        args.input = Some(prompt_path("Path to the extracted Takeout folder")?);
    }
    if args.output.is_none() {
        args.output = Some(prompt_path("Path to the output folder")?);
    }
    if !args.flat && !args.divide_to_dates && !args.no_divide_to_dates {
        loop {
            match prompt("Layout [1] one folder, [2] year/month (default 1)")?.as_str() {
                "" | "1" => break,
                "2" => {
                    args.divide_to_dates = true;
                    break;
                }
                _ => eprintln!("Enter 1 or 2."),
            }
        }
    }
    if args.albums.is_none() {
        println!("Albums [0] shortcut, [1] duplicate-copy, [2] json, [3] nothing");
        loop {
            let answer = prompt("Album mode (default shortcut)")?;
            let modes = Albums::value_variants();
            let mode = if answer.is_empty() {
                Some(Albums::Shortcut)
            } else if let Ok(index) = answer.parse::<usize>() {
                modes.get(index).copied()
            } else {
                Albums::from_str(&answer, true).ok()
            };
            if let Some(mode) = mode {
                args.albums = Some(mode);
                break;
            }
            eprintln!("Enter a mode from the list.");
        }
    }
    Ok(())
}

fn prompt_path(label: &str) -> Result<PathBuf> {
    loop {
        let answer = prompt(label)?;
        let path = answer
            .strip_prefix('"')
            .and_then(|path| path.strip_suffix('"'))
            .unwrap_or(&answer);
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
        eprintln!("Enter a folder path.");
    }
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}: ");
    io::stdout().flush()?;
    let mut line = String::new();
    if io::stdin().read_line(&mut line)? == 0 {
        bail!("Input ended while waiting for a response");
    }
    Ok(line.trim().to_owned())
}

fn prepare_output(output: &Path, input: &Path) -> Result<bool> {
    let mut existing = Vec::new();
    for entry in fs::read_dir(output)? {
        let path = entry?.path();
        // An output folder may contain the extracted Takeout as a descendant.
        if !input.starts_with(&path) {
            existing.push(path);
        }
    }
    if existing.is_empty() {
        return Ok(true);
    }
    println!("Output folder is not empty: {}", output.display());
    loop {
        match prompt("[1] clear output, [2] keep existing files, [3] exit")?.as_str() {
            "1" => {
                for path in existing {
                    remove_output_entry(&path)
                        .with_context(|| format!("Removing {}", path.display()))?;
                }
                return Ok(true);
            }
            "2" => return Ok(true),
            "3" => return Ok(false),
            _ => eprintln!("Enter 1, 2, or 3."),
        }
    }
}

fn remove_output_entry(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() && !output::is_link(&metadata) {
        return fs::remove_dir_all(path);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        if metadata.is_dir() || metadata.file_type().is_symlink_dir() {
            return fs::remove_dir(path);
        }
    }
    fs::remove_file(path)
}
