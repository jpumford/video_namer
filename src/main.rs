use serde::Deserialize;
use std::path::{Path, PathBuf};
use clap::{Parser, Subcommand};
use anyhow::{anyhow, Result};
use clap_verbosity_flag::Verbosity;
use ffmpeg_next::codec;
use ffmpeg_next::media::Type;
use ffmpeg_next::util::frame::video::Video;
use glob::MatchOptions;
use image::{ImageBuffer, RgbImage};
use indicatif::ProgressBar;
use inquire::Select;
use ocrs::ImageSource;
use tracing::info;
use tracing::debug;

/// a program that finds title cards for a show about a blue dog
#[derive(Parser)]
struct Args {
    #[clap(flatten)]
    verbose: Verbosity,

    #[clap(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    EpisodeName {
        #[clap(short, long)]
        path: String,

        #[clap(short, long)]
        output: String,
    },
    Ocr {
        #[clap(short, long)]
        path: String,
    },
    RenameAll {
        pattern: String,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_max_level(args.verbose.tracing_level_filter())
        .init();

    match args.command {
        Commands::EpisodeName { path, output } => episode_name(&path, &output),
        Commands::Ocr { path } => ocr(&path),
        Commands::RenameAll { pattern } => rename_all(&pattern),
    }
}

fn rename_all(pattern: &str) -> Result<()> {
    let episodes = get_episode_names("bluey.csv")?;
    let mut files = glob::glob_with(pattern, MatchOptions {
        case_sensitive: false,
        require_literal_separator: false,
        require_literal_leading_dot: true,
    })?.map(|x| x.map_err(|x| anyhow!(x.to_string()))).collect::<Result<Vec<_>>>()?;

    let len = files.len();
    for (i, file) in files.iter_mut().enumerate() {
        info!("File {} of {}: {:?}", i, len, file);

        let filename = file.file_name().map(|x| x.to_string_lossy()).ok_or(anyhow!("file has no file_name"))?;

        let blue_frame = extract_frames(file)?;
        if let Some((frame, _)) = blue_frame {
            debug!("found a blue frame");
            let name = get_episode_name(&frame)?;
            debug!(name, "episode name");
            let corrected = get_corrected_episode_name(&name, &episodes).unwrap();
            debug!(corrected = corrected.name, "corrected episode name");

            let new_filename = format!("Bluey - {} - {}.mkv", corrected.season_and_episode, corrected.name);
            info!("Renaming {} to {}", filename, new_filename);
            let new_path = file.parent().unwrap().join(new_filename);
            std::fs::rename(file, new_path)?;
        } else {
            debug!("no blue frame found for {}", filename);
        }
    }
    Ok(())
}

fn ocr(path: &str) -> Result<()> {
    let image = image::open(path)?.into_rgb8();
    let name = get_episode_name(&image)?;
    info!(name, "episode name");
    let episodes = get_episode_names("bluey.csv")?;
    debug!(len = episodes.len(), "episodes loaded");
    let lowest = episodes.iter().min_by_key(|episode| {
        // TODO: other distances?
        strsim::levenshtein(&episode.name, &name) as usize
    }).ok_or(anyhow!("No episode found"))?;

    info!(lowest.name, lowest.season_and_episode, "closest episode");
    Ok(())
}

fn episode_name(path: &str, output: &str) -> Result<()> {
    let blue_frame = extract_frames(Path::new(path))?;

    if let Some((frame, index)) = blue_frame {
        info!(index, "found a blue frame");
        // write frame to output
        frame.save(output)?;

        let name = get_episode_name(&frame)?;
        info!(name, "episode name");

        let episodes = get_episode_names("bluey.csv")?;
        debug!(len = episodes.len(), "episodes loaded");
        let lowest = episodes.iter().min_by_key(|episode| {
            // TODO: other distances?
            strsim::normalized_levenshtein(&episode.name, &name) as usize
        }).ok_or(anyhow!("No episode found"))?;

        info!(lowest.name, lowest.season_and_episode, "closest episode");
    } else {
        info!("no blue frame found");
    }

    Ok(())
}

fn extract_frames(filename: &Path) -> Result<Option<(RgbImage, usize)>> {
    let mut ictx = ffmpeg_next::format::input(filename)?;
    let stream = ictx.streams().best(Type::Video).ok_or(anyhow!("Unable to decode"))?;
    let index = stream.index();

    let decoder_context = codec::context::Context::from_parameters(stream.parameters())?;
    let mut decoder = decoder_context.decoder().video()?;

    let mut scaler = ffmpeg_next::software::scaling::context::Context::get(
        decoder.format(),
        decoder.width(),
        decoder.height(),
        ffmpeg_next::format::Pixel::RGB24,
        decoder.width(),
        decoder.height(),
        ffmpeg_next::software::scaling::flag::Flags::BILINEAR,
    )?;


    let mut frame_index = 0;
    let bar = ProgressBar::new(14400);

    let mut receive_and_process_decoded_frames =
        |decoder: &mut ffmpeg_next::decoder::Video| -> Result<Option<(RgbImage, usize)>> {
            let mut decoded = Video::empty();
            while decoder.receive_frame(&mut decoded).is_ok() {
                if frame_index > 900 && frame_index % 30 == 0 {
                    let mut rgb_frame = Video::empty();
                    scaler.run(&decoded, &mut rgb_frame)?;
                    if let Some(img) = is_blue_dominant(&rgb_frame)? {
                        return Ok(Some((img, frame_index)));
                    }
                }
                frame_index += 1;
                bar.inc(1);
            }
            Ok(None)
        };

    for (stream, packet) in ictx.packets() {
        if stream.index() == index {
            decoder.send_packet(&packet)?;
            if let Some(blue_frame) = receive_and_process_decoded_frames(&mut decoder)? {
                return Ok(Some(blue_frame));
            }
        }
    }
    decoder.send_eof()?;
    if let Some(blue_frame) = receive_and_process_decoded_frames(&mut decoder)? {
        return Ok(Some(blue_frame));
    }

    Ok(None)
}

// Check if the frame is mostly blue
fn is_blue_dominant(frame: &Video) -> Result<Option<RgbImage>> {
    let width = frame.width();
    let height = frame.height();
    let data = frame.data(0);

    let img: RgbImage = ImageBuffer::from_raw(width, height, data.to_vec()).ok_or(anyhow!("couldn't decode image"))?;

    let mut blue_pixels = 0;
    let mut total_pixels = 0;

    for pixel in img.pixels() {
        let [r, g, b] = pixel.0;
        if b > 230 && r < 180 && g < 235 { // Simple blue detection
            blue_pixels += 1;
        }
        total_pixels += 1;
    }

    if (blue_pixels as f64 / total_pixels as f64) > 0.8 {
        return Ok(Some(img));
    }

    Ok(None)
}

fn file_path(path: &str) -> PathBuf {
    let mut abs_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    abs_path.push(path);
    abs_path
}

fn get_corrected_episode_name(candiate_name: &str, episodes: &[Episode]) -> Option<Episode> {
    episodes.iter().min_by_key(|episode| {
        strsim::levenshtein(&episode.name, candiate_name) as usize
    }).cloned()
}

fn get_episode_name(frame: &RgbImage) -> Result<String> {
    let detection_model_path = file_path("text-detection.rten");
    let rec_model_path = file_path("text-recognition.rten");

    let detection_model = rten::Model::load_file(detection_model_path)?;
    let recognition_model = rten::Model::load_file(rec_model_path)?;

    let engine = ocrs::OcrEngine::new(ocrs::OcrEngineParams {
        detection_model: Some(detection_model),
        recognition_model: Some(recognition_model),
        ..Default::default()
    })?;

    let img_source = ImageSource::from_bytes(frame.as_raw(), frame.dimensions())?;
    let ocr_input = engine.prepare_input(img_source)?;

    let word_rects = engine.detect_words(&ocr_input)?;
    debug!(len = word_rects.len(), "detected words");
    let line_rects = engine.find_text_lines(&ocr_input, &word_rects);
    debug!(len = line_rects.len(), "detected lines");
    let line_texts = engine.recognize_text(&ocr_input, &line_rects)?;

    let lines = line_texts.iter().flatten().map(|x| x.to_string()).filter(|x| x.len() > 1).collect::<Vec<_>>();
    debug!("{:#?}", lines);
    match &lines[..] {
        [] => Err(anyhow!("No text detected")),
        [text] => Ok(text.to_string()),
        options => {
            Ok(Select::new("Choose an OCR option:", options.to_vec())
                .prompt()?.to_string())
        },
    }
}


#[derive(Debug, Deserialize, Clone)]
struct Episode {
    name: String,
    #[serde(rename = "season")]
    season_and_episode: String,
}

fn get_episode_names(path: &str) -> Result<Vec<Episode>> {
    let mut rdr = csv::Reader::from_reader(std::fs::File::open(path)?);
    rdr.deserialize().collect::<csv::Result<Vec<Episode>>>().map_err(|e| anyhow!(e.to_string()))
}