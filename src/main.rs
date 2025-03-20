use clap::Parser;
use anyhow::{anyhow, Result};
use ffmpeg_next::codec;
use ffmpeg_next::media::Type;
use ffmpeg_next::util::frame::video::Video;
use image::{ImageBuffer, RgbImage};
use indicatif::ProgressBar;
use log::info;

/// a program that finds title cards for a show about a blue dog
#[derive(Parser)]
struct Args {
    #[clap(short, long)]
    path: String,

    #[clap(short, long)]
    output: String,
}

fn main() -> Result<()> {
    let args = Args::parse();


    let blue_frame = extract_frames(&args.path)?;

    if let Some((frame, index)) = blue_frame {
        info!("found a blue frame at frame count {}", index);
        // write frame to output
        frame.save(&args.output)?;
    } else {
        info!("no blue frame found");
    }

    Ok(())
}

fn extract_frames(filename: &str) -> Result<Option<(RgbImage, usize)>> {
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
                bar.finish();
                return Ok(Some(blue_frame));
            }
        }
    }
    decoder.send_eof()?;
    if let Some(blue_frame) = receive_and_process_decoded_frames(&mut decoder)? {
        bar.finish();
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