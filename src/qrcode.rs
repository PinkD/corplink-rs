use qrcode::{EcLevel, QrCode, Version};
use terminal_graphics::Colour;
use terminal_graphics::Display;

const QR_MIN_VERSION: i16 = 20;
const QR_MAX_VERSION: i16 = 40;
const QR_VERSION_STEP: i16 = 4;

#[derive(Clone)]
pub struct TerminalQrCode {
    code: QrCode,
}

impl TerminalQrCode {
    pub fn from_bytes<D: AsRef<[u8]>>(data: D) -> Result<TerminalQrCode, anyhow::Error> {
        let data = data.as_ref();
        let mut v = QR_MIN_VERSION;
        loop {
            if let Ok(code) = QrCode::with_version(data, Version::Normal(v), EcLevel::L) {
                return Ok(TerminalQrCode { code });
            }
            if v >= QR_MAX_VERSION {
                break;
            }
            v += QR_VERSION_STEP;
        }
        Err(anyhow::anyhow!("data too long for QR code (max version {QR_MAX_VERSION})"))
    }

    pub fn print(&self) {
        let code = self.code.clone();
        let width = code.width();
        let height = (width as f32 / 2_f32).ceil() as usize;
        let pixels = code.to_colors();
        let mut display = Display::new((width + 2) as u32, (height + 1) as u32);

        for (i, pixel) in pixels.iter().enumerate() {
            let x = i % width;
            let y = (i as f32 / width as f32).floor() as isize;

            let colour = match pixel {
                qrcode::types::Color::Light => Colour::White,
                qrcode::types::Color::Dark => Colour::Black,
            };

            let char_colour = match width % 2 {
                0 => colour,
                1 => Colour::White,
                _ => Colour::White,
            };

            match y % 2 {
                0 => display.set_pixel(x as isize + 1, (y / 2) + 1, '▄', char_colour, colour),
                1 => display
                    .get_mut_pixel(x as isize + 1, ((y - 1) / 2) + 1)
                    .set_colour(colour),
                _ => {
                    log::warn!("failed to print QR code");
                    return;
                }
            }
        }
        for _i in 0..((height + 2) as u32) {
            println!();
        }
        display.print();
    }
}
