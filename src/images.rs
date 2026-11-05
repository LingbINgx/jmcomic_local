use anyhow::{Context, Result, bail};
use image::{DynamicImage, GenericImage, ImageFormat, ImageReader, Limits, RgbImage};
use std::{io::Cursor, path::Path};

pub fn decode(bytes: &[u8]) -> Result<DynamicImage> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(32768);
    limits.max_image_height = Some(65536);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().context("图片不能正常解码")
}

pub fn check(path: &Path) -> Result<()> {
    decode(&std::fs::read(path)?)?;
    Ok(())
}

pub fn segments(scramble_id: u128, photo_id: u128, stem: &str) -> u32 {
    if photo_id < scramble_id {
        return 0;
    }
    if photo_id < 268850 {
        return 10;
    }
    let digest = format!("{:x}", md5::compute(format!("{photo_id}{stem}")));
    let modulo = if photo_id < 421926 { 10 } else { 8 };
    (u32::from(*digest.as_bytes().last().unwrap()) % modulo) * 2 + 2
}

pub fn restore(source: DynamicImage, parts: u32) -> Result<DynamicImage> {
    if parts == 0 {
        return Ok(source);
    }
    let source = source.to_rgb8();
    let (width, height) = source.dimensions();
    if height < parts {
        bail!("图片高度小于分块数，无法安全还原");
    }
    let mut result = RgbImage::new(width, height);
    let remainder = height % parts;
    let block = height / parts;
    for index in 0..parts {
        let src_y = height - block * (index + 1) - remainder;
        let dst_y = block * index + if index == 0 { 0 } else { remainder };
        let rows = block + if index == 0 { remainder } else { 0 };
        result.copy_from(
            &image::imageops::crop_imm(&source, 0, src_y, width, rows).to_image(),
            0,
            dst_y,
        )?;
    }
    Ok(DynamicImage::ImageRgb8(result))
}

pub fn processed(bytes: &[u8], name: &str, parts: u32) -> Result<Vec<u8>> {
    let decoded = decode(bytes)?;
    // GIF animations must retain all frames, as in the original downloader.
    if parts == 0 || name.to_ascii_lowercase().ends_with(".gif") {
        return Ok(bytes.to_vec());
    }
    let restored = restore(decoded, parts)?;
    let format = ImageFormat::from_path(name).context("不支持的图片扩展名")?;
    let mut output = Cursor::new(Vec::new());
    restored.write_to(&mut output, format)?;
    let output = output.into_inner();
    decode(&output)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restores_uneven_blocks() {
        let mut source = RgbImage::new(1, 7);
        for (y, pixel) in [5, 6, 3, 4, 0, 1, 2].into_iter().enumerate() {
            source.put_pixel(0, y as u32, image::Rgb([pixel, 0, 0]));
        }
        let result = restore(source.into(), 3).unwrap().to_rgb8();
        assert_eq!(
            result.pixels().map(|p| p[0]).collect::<Vec<_>>(),
            [0, 1, 2, 3, 4, 5, 6]
        );
        assert!(restore(RgbImage::new(1, 2).into(), 4).is_err());
    }
    #[test]
    fn handles_algorithm_boundaries() {
        assert_eq!(segments(220980, 220979, "00001"), 0);
        assert_eq!(segments(220980, 220980, "00001"), 10);
        for id in [268850, 421925, 421926] {
            let n = segments(220980, id, "00001");
            assert!(n >= 2 && n <= if id < 421926 { 20 } else { 16 } && n.is_multiple_of(2));
        }
        assert!(decode(b"truncated image").is_err());
    }
}
