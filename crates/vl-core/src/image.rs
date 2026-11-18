//! A plain BGR8 image (the layout ffmpeg's `bgr24` gives).
//! Processing lives in vl-engine; this is just the shared container + PNG/JPEG IO.
use std::path::Path;

#[derive(Clone, Default, PartialEq)]
pub struct BgrImage {
    pub w: usize,
    pub h: usize,
    /// row-major, 3 bytes per pixel (B, G, R)
    pub data: Vec<u8>,
}

impl std::fmt::Debug for BgrImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BgrImage({}x{})", self.w, self.h)
    }
}

impl BgrImage {
    pub fn new(w: usize, h: usize) -> Self {
        BgrImage { w, h, data: vec![0; w * h * 3] }
    }
    pub fn from_raw(w: usize, h: usize, data: Vec<u8>) -> Self {
        assert_eq!(data.len(), w * h * 3);
        BgrImage { w, h, data }
    }
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
    #[inline]
    pub fn px(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y * self.w + x) * 3;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }
    #[inline]
    pub fn set(&mut self, x: usize, y: usize, p: [u8; 3]) {
        let i = (y * self.w + x) * 3;
        self.data[i..i + 3].copy_from_slice(&p);
    }
    /// Copy of the rectangle (x, y, w, h), clamped to the image.
    pub fn crop(&self, x: i64, y: i64, w: i64, h: i64) -> BgrImage {
        let x0 = x.clamp(0, self.w as i64) as usize;
        let y0 = y.clamp(0, self.h as i64) as usize;
        let x1 = (x + w).clamp(0, self.w as i64) as usize;
        let y1 = (y + h).clamp(0, self.h as i64) as usize;
        let (cw, ch) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
        let mut out = Vec::with_capacity(cw * ch * 3);
        for yy in y0..y1 {
            let i = (yy * self.w + x0) * 3;
            out.extend_from_slice(&self.data[i..i + cw * 3]);
        }
        BgrImage { w: cw, h: ch, data: out }
    }
    pub fn to_rgb(&self) -> Vec<u8> {
        let mut out = self.data.clone();
        for p in out.as_chunks_mut::<3>().0 {
            p.swap(0, 2);
        }
        out
    }
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.w * self.h * 4);
        for p in self.data.as_chunks::<3>().0 {
            out.extend_from_slice(&[p[2], p[1], p[0], 255]);
        }
        out
    }
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        image::save_buffer(path, &self.to_rgb(), self.w as u32, self.h as u32, image::ColorType::Rgb8)?;
        Ok(())
    }
    pub fn load(path: &Path) -> anyhow::Result<BgrImage> {
        let img = image::open(path)?.to_rgb8();
        let (w, h) = (img.width() as usize, img.height() as usize);
        let mut data = img.into_raw();
        for p in data.as_chunks_mut::<3>().0 {
            p.swap(0, 2);
        }
        Ok(BgrImage { w, h, data })
    }
}
