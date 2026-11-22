//! Lossless per-frame alpha. Never infer transparency from RGB (black is content).
use anyhow::{ensure, Result};
use std::sync::Arc;

#[derive(Default)]
pub(super) struct Mask {
    size: (u16, u16),
    previous: Vec<u8>,
    png: Option<Arc<Vec<u8>>>,
}
impl Mask {
    pub(super) fn encode(
        &mut self,
        pixels: &[u8],
        width: u16,
        height: u16,
    ) -> Result<Option<Arc<Vec<u8>>>> {
        let count = usize::from(width) * usize::from(height);
        ensure!(
            count > 0 && count <= 16_777_216 && pixels.len() == count * 4,
            "invalid alpha frame dimensions"
        );
        if pixels.chunks_exact(4).all(|p| p[3] == 255) {
            self.previous.clear();
            self.png = None;
            self.size = (width, height);
            return Ok(None);
        }
        if self.size == (width, height)
            && self.previous.len() == count
            && pixels
                .chunks_exact(4)
                .zip(&self.previous)
                .all(|(p, a)| p[3] == *a)
        {
            return Ok(self.png.clone());
        }
        let alpha: Vec<u8> = pixels.chunks_exact(4).map(|p| p[3]).collect();
        let mut bytes = Vec::new();
        {
            let mut png = png::Encoder::new(&mut bytes, width.into(), height.into());
            png.set_color(png::ColorType::Grayscale);
            png.set_depth(png::BitDepth::Eight);
            png.set_compression(png::Compression::Fast);
            png.write_header()?.write_image_data(&alpha)?;
        }
        ensure!(bytes.len() <= 8 * 1024 * 1024, "alpha mask exceeds 8 MiB");
        self.size = (width, height);
        self.previous = alpha;
        self.png = Some(Arc::new(bytes));
        Ok(self.png.clone())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transparent_black_and_opaque_black_are_distinct_and_masks_follow_resize() {
        let mut mask = Mask::default();
        let mut bgra = vec![0, 0, 0, 255, 30, 40, 50, 128, 0, 0, 0, 0, 1, 2, 3, 255];
        let one = mask.encode(&bgra, 2, 2).unwrap().unwrap();
        let decoder = png::Decoder::new(one.as_slice());
        let mut reader = decoder.read_info().unwrap();
        let mut out = vec![0; reader.output_buffer_size()];
        reader.next_frame(&mut out).unwrap();
        assert_eq!(out, vec![255, 128, 0, 255]);
        bgra[0] = 20;
        assert!(Arc::ptr_eq(
            &one,
            &mask.encode(&bgra, 2, 2).unwrap().unwrap()
        ));
        let resized = mask.encode(&bgra, 4, 1).unwrap().unwrap();
        assert!(!Arc::ptr_eq(&one, &resized));
        for p in bgra.chunks_exact_mut(4) {
            p[3] = 255;
        }
        assert!(mask.encode(&bgra, 2, 2).unwrap().is_none());
        bgra[3] = 0;
        assert!(mask.encode(&bgra, 2, 2).unwrap().is_some());
        assert!(mask.encode(&bgra, 0, 2).is_err());
        assert!(mask.encode(&bgra, 2, 3).is_err());
    }
}
