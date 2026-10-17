use crate::{Gpu, Image};

impl Gpu {
    /// Stream tightly packed RGBA32F rows through one reusable staging buffer.
    /// The requested band is capped at 256 rows and the device buffer limit.
    pub fn download_rows(&self, img: &Image, band_rows: u32, mut f: impl FnMut(u32, u32, &[f32]) -> anyhow::Result<()>) -> anyhow::Result<()> {
        anyhow::ensure!(band_rows > 0, "readback bands must be nonempty");
        let rows = band_rows.min(256).min(Self::transfer_rows(img.width(), 16, self.device.limits().max_buffer_size)).min(img.height());
        let pitch = (img.width() * 16).div_ceil(256) * 256;
        let buffer = self.readback_buffer(u64::from(pitch) * u64::from(rows));
        let mut pixels = vec![0.0; img.width() as usize * rows as usize * 4];
        for y in (0..img.height()).step_by(rows as usize) {
            let height = rows.min(img.height() - y);
            let band = &mut pixels[..img.width() as usize * height as usize * 4];
            self.readback_band(img, y, height, pitch, &buffer, band)?;
            f(y, height, band)?;
        }
        Ok(())
    }

    /// Pull a row range into caller-owned storage, using at most 256 staging rows.
    pub fn download_row_range(&self, img: &Image, y0: u32, rows: u32, out: &mut [f32]) -> anyhow::Result<()> {
        anyhow::ensure!(y0 <= img.height() && rows <= img.height() - y0, "invalid readback row range");
        anyhow::ensure!(out.len() == img.width() as usize * rows as usize * 4, "invalid readback destination length");
        if rows == 0 { return Ok(()); }
        let band_rows = rows.min(256).min(Self::transfer_rows(img.width(), 16, self.device.limits().max_buffer_size));
        let pitch = (img.width() * 16).div_ceil(256) * 256;
        let buffer = self.readback_buffer(u64::from(pitch) * u64::from(band_rows));
        let stride = img.width() as usize * 4;
        for y in (0..rows).step_by(band_rows as usize) {
            let height = band_rows.min(rows - y);
            self.readback_band(img, y0 + y, height, pitch, &buffer, &mut out[y as usize * stride..(y + height) as usize * stride])?;
        }
        Ok(())
    }

    fn readback_buffer(&self, size: u64) -> crate::TrackedBuffer {
        self.track_buffer(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Moya bounded readback"), size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        }))
    }

    fn readback_band(&self, img: &Image, y: u32, rows: u32, pitch: u32, buffer: &wgpu::Buffer, out: &mut [f32]) -> anyhow::Result<()> {
        self.record(None, |encoder| {
            let mut source = img.texture.as_image_copy();
            source.origin.y = y;
            encoder.copy_texture_to_buffer(source, wgpu::TexelCopyBufferInfo {
                buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(pitch), rows_per_image: Some(rows) },
            }, wgpu::Extent3d { width: img.width(), height: rows, depth_or_array_layers: 1 });
        });
        self.flush();
        let (tx, rx) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| { let _ = tx.send(result); });
        self.device.poll(wgpu::PollType::wait_indefinitely())?;
        rx.recv()??;
        let bytes = buffer.slice(..).get_mapped_range()?;
        for (target, source) in out.chunks_exact_mut(img.width() as usize * 4).zip(bytes.chunks_exact(pitch as usize)) {
            target.copy_from_slice(bytemuck::cast_slice(&source[..img.width() as usize * 16]));
        }
        drop(bytes);
        buffer.unmap();
        Ok(())
    }

    /// Upload 16-bit mask codes as exactly `code as f32 / 65535.0` grayscale.
    /// CPU expansion is bounded to 256 rows, not the full image.
    pub fn upload_gray_u16(&self, w: u32, h: u32, gray: &[u16]) -> Image {
        assert_eq!(gray.len(), w as usize * h as usize);
        self.flush();
        let image = self.image(w, h);
        let rows = 256.min(h).min(Self::transfer_rows(w, 16, self.device.limits().max_buffer_size));
        let mut rgba = vec![0.0; w as usize * rows as usize * 4];
        for y in (0..h).step_by(rows as usize) {
            let height = rows.min(h - y);
            let band = &mut rgba[..w as usize * height as usize * 4];
            for (p, &value) in band.chunks_exact_mut(4).zip(&gray[y as usize * w as usize..(y + height) as usize * w as usize]) {
                let v = value as f32 / 65535.0;
                p.copy_from_slice(&[v, v, v, 1.0]);
            }
            let mut target = image.texture.as_image_copy();
            target.origin.y = y;
            self.upload_bands(target, wgpu::Extent3d { width: w, height, depth_or_array_layers: 1 }, 16, band, self.device.limits().max_buffer_size);
        }
        image
    }

    /// Quantize the red channel with the same clamp/round as CPU mask history.
    pub fn download_gray_u16(&self, img: &Image) -> Vec<u16> {
        let mut gray = Vec::with_capacity(img.width() as usize * img.height() as usize);
        self.download_rows(img, 256, |_, _, rgba| {
            gray.extend(rgba.chunks_exact(4).map(|p| (p[0].clamp(0.0, 1.0) * 65535.0).round() as u16));
            Ok(())
        }).expect("GPU mask readback failed");
        gray
    }
}
