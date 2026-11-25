pub mod client;

use image::codecs::bmp::BmpEncoder;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::imageops::FilterType;
use image::{ColorType, DynamicImage, ImageEncoder, RgbaImage};
use serde::{Deserialize, Serialize};

pub const MAX_METADATA_BYTES: usize = 4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ImageFormatKind {
    Bmp = 0,
    Gif = 1,
    Jpg = 2,
    Png = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ImageQuantizationKind {
    BlackAndWhite1bpp = 0,
    Grayscale2bpp = 1,
    Grayscale4bpp = 2,
    Grayscale8bpp = 3,
    Color8bpp = 4,
    Color16bpp = 5,
    Color24bpp = 6,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataInput {
    pub capture_application_data: bool,
    pub capture_browser_data: bool,
    pub capture_window_title: bool,
    pub app_name: String,
    pub is_main_window: bool,
    pub window_title: String,
    pub browser_url: String,
    pub trigger_action: String,
}

pub struct EncodedScreenshot {
    pub format_name: String,
    pub bytes: Vec<u8>,
}

fn quantize_levels(v: u8, levels: u32) -> u8 {
    let step = ((v as u32) * (levels - 1) + 127) / 255;
    ((step * 255) / (levels - 1)) as u8
}

pub fn process_and_encode_rgba(
    rgba: &[u8],
    width: u32,
    height: u32,
    downscaling: f64,
    quantization: ImageQuantizationKind,
    format: ImageFormatKind,
) -> Option<EncodedScreenshot> {
    if width == 0 || height == 0 || rgba.len() < (width as usize * height as usize * 4) {
        return None;
    }

    let img = RgbaImage::from_raw(width, height, rgba[..(width * height * 4) as usize].to_vec())?;
    let mut dyn_img = DynamicImage::ImageRgba8(img);

    if downscaling > 1.0 {
        let tw = ((width as f64) / downscaling).max(16.0) as u32;
        let th = ((height as f64) / downscaling).max(16.0) as u32;
        dyn_img = dyn_img.resize(tw, th, FilterType::Triangle);
    }

    let is_gray = matches!(
        quantization,
        ImageQuantizationKind::BlackAndWhite1bpp
            | ImageQuantizationKind::Grayscale2bpp
            | ImageQuantizationKind::Grayscale4bpp
            | ImageQuantizationKind::Grayscale8bpp
    );

    let (w, h, color_type, raw_pixels): (u32, u32, ColorType, Vec<u8>) = if is_gray {
        let mut gray = dyn_img.to_luma8();
        match quantization {
            ImageQuantizationKind::BlackAndWhite1bpp => {
                for p in gray.pixels_mut() {
                    p.0[0] = if p.0[0] >= 128 { 255 } else { 0 };
                }
            }
            ImageQuantizationKind::Grayscale2bpp => {
                for p in gray.pixels_mut() {
                    p.0[0] = quantize_levels(p.0[0], 4);
                }
            }
            ImageQuantizationKind::Grayscale4bpp => {
                for p in gray.pixels_mut() {
                    p.0[0] = quantize_levels(p.0[0], 16);
                }
            }
            _ => {}
        }
        (gray.width(), gray.height(), ColorType::L8, gray.into_raw())
    } else {
        let mut rgb = dyn_img.to_rgb8();
        match quantization {
            ImageQuantizationKind::Color8bpp => {
                for p in rgb.pixels_mut() {
                    p.0[0] = quantize_levels(p.0[0], 8);
                    p.0[1] = quantize_levels(p.0[1], 8);
                    p.0[2] = quantize_levels(p.0[2], 4);
                }
            }
            ImageQuantizationKind::Color16bpp => {
                for p in rgb.pixels_mut() {
                    p.0[0] = (p.0[0] & 0xf8) | (p.0[0] >> 5);
                    p.0[1] = (p.0[1] & 0xfc) | (p.0[1] >> 6);
                    p.0[2] = (p.0[2] & 0xf8) | (p.0[2] >> 5);
                }
            }
            _ => {}
        }
        (rgb.width(), rgb.height(), ColorType::Rgb8, rgb.into_raw())
    };

    let mut out = Vec::new();
    let format_name = match format {
        ImageFormatKind::Jpg => {
            let mut enc = JpegEncoder::new_with_quality(&mut out, 75);
            enc.encode(&raw_pixels, w, h, color_type.into()).ok()?;
            "jpg".to_string()
        }
        ImageFormatKind::Bmp => {
            let mut enc = BmpEncoder::new(&mut out);
            enc.encode(&raw_pixels, w, h, color_type.into()).ok()?;
            "bmp".to_string()
        }
        ImageFormatKind::Png | ImageFormatKind::Gif => {
            let enc = PngEncoder::new(&mut out);
            enc.write_image(&raw_pixels, w, h, color_type.into()).ok()?;
            "png".to_string()
        }
    };

    Some(EncodedScreenshot {
        format_name,
        bytes: out,
    })
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    if max_chars <= 3 {
        return s.chars().take(max_chars).collect();
    }
    let mut out: String = s.chars().take(max_chars - 3).collect();
    out.push_str("...");
    out
}

pub fn build_metadata_json(input: &MetadataInput) -> Vec<u8> {
    let mut executable = if input.capture_application_data {
        format!("safe-exam-browser ({})", input.app_name)
    } else {
        String::new()
    };

    let effective_title = if input.window_title.trim().is_empty() {
        "Safe Exam Browser".to_string()
    } else {
        input.window_title.clone()
    };

    let mut browser_info = if input.capture_browser_data {
        let prefix = if input.is_main_window {
            "Main Window: "
        } else {
            "Additional Window: "
        };
        format!("{prefix}{effective_title}")
    } else {
        String::new()
    };

    let mut browser_url = if input.capture_browser_data {
        input.browser_url.clone()
    } else {
        String::new()
    };

    let mut window_title = if input.capture_window_title {
        effective_title
    } else {
        String::new()
    };

    let mut user_action = if input.trigger_action.trim().is_empty() {
        "Interval".to_string()
    } else {
        input.trigger_action.trim().to_string()
    };

    let serialize = |exe: &str, br: &str, url: &str, act: &str, wt: &str| -> Vec<u8> {
        let mut map = serde_json::Map::new();
        if !exe.is_empty() {
            map.insert("executable".into(), exe.into());
        }
        if !br.is_empty() {
            map.insert("screenProctoringMetadataBrowser".into(), br.into());
        }
        if !url.is_empty() {
            map.insert("screenProctoringMetadataURL".into(), url.into());
        }
        if !act.is_empty() {
            map.insert("screenProctoringMetadataUserAction".into(), act.into());
        }
        if !wt.is_empty() {
            map.insert("screenProctoringMetadataWindowTitle".into(), wt.into());
        }
        serde_json::Value::Object(map).to_string().into_bytes()
    };

    let encoded = serialize(&executable, &browser_info, &browser_url, &user_action, &window_title);
    if encoded.len() <= MAX_METADATA_BYTES {
        return encoded;
    }

    browser_url = truncate_chars(&browser_url, 1024);
    browser_info = truncate_chars(&browser_info, 512);
    window_title = truncate_chars(&window_title, 512);
    user_action = truncate_chars(&user_action, 256);
    executable = truncate_chars(&executable, 256);
    let encoded = serialize(&executable, &browser_info, &browser_url, &user_action, &window_title);
    if encoded.len() <= MAX_METADATA_BYTES {
        return encoded;
    }

    browser_url = truncate_chars(&browser_url, 256);
    browser_info = truncate_chars(&browser_info, 128);
    window_title = truncate_chars(&window_title, 128);
    serialize(&executable, &browser_info, &browser_url, &user_action, &window_title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_downscaled_quantized_png_and_jpg() {
        let rgba = vec![120u8; 64 * 64 * 4];
        let png = process_and_encode_rgba(
            &rgba,
            64,
            64,
            2.0,
            ImageQuantizationKind::Grayscale4bpp,
            ImageFormatKind::Png,
        )
        .unwrap();
        assert_eq!(png.format_name, "png");
        assert!(png.bytes.starts_with(b"\x89PNG"));

        let jpg = process_and_encode_rgba(
            &rgba,
            64,
            64,
            1.0,
            ImageQuantizationKind::Color24bpp,
            ImageFormatKind::Jpg,
        )
        .unwrap();
        assert_eq!(jpg.format_name, "jpg");
        assert!(jpg.bytes.starts_with(&[0xff, 0xd8]));
    }

    #[test]
    fn metadata_enforces_4000_byte_limit_with_valid_json() {
        let input = MetadataInput {
            capture_application_data: true,
            capture_browser_data: true,
            capture_window_title: true,
            app_name: "Safe Exam Browser".into(),
            is_main_window: true,
            window_title: "T".repeat(3000),
            browser_url: format!("https://example.org/{}", "a".repeat(3000)),
            trigger_action: "Click".into(),
        };
        let bytes = build_metadata_json(&input);
        assert!(bytes.len() <= MAX_METADATA_BYTES);
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(parsed.is_object());
    }
}
