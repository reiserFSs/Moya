use super::{depth, BlendMode, Depth, Gpu, Image};

/// A nonempty rectangle in the full output's pixel grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region { pub x: u32, pub y: u32, pub w: u32, pub h: u32 }

/// Source outer-edge coordinates; destination pixel centers map to
/// `origin + (pixel + 0.5) * src_per_dst` in source outer-edge coordinates.
#[derive(Clone, Copy, Debug)]
pub struct ViewMap { pub origin_x: f64, pub origin_y: f64, pub src_per_dst: f64 }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter { Nearest, Bilinear }

impl Region {
    fn validate(self, w: u32, h: u32) {
        assert!(self.w > 0 && self.h > 0 && self.x < w && self.y < h
            && self.w <= w - self.x && self.h <= h - self.y, "region outside full output");
    }
}

impl Gpu {
    fn run_region(&self, op: u32, inputs: &[&Image], args: &[f32], full_w: u32, full_h: u32, region: Region) -> Image {
        region.validate(full_w, full_h);
        let mut params = [0.; 28];
        params[..args.len()].copy_from_slice(args);
        params[24..].copy_from_slice(&[full_w as f32, full_h as f32, region.x as f32, region.y as f32]);
        self.run(op, inputs, region.w, region.h, &params, &[])
    }

    fn point_region(&self, op: u32, inputs: &[&Image], args: &[f32], full_w: u32, full_h: u32, region: Region) -> Image {
        for input in inputs {
            assert!((input.width(), input.height()) == (full_w, full_h)
                || (input.width(), input.height()) == (region.w, region.h), "pointwise region input must be full or region-sized");
        }
        self.run_region(op, inputs, args, full_w, full_h, region)
    }

    /// Pointwise region operations accept full-grid or already cropped inputs.
    pub fn copy_region(&self, img: &Image, full_w: u32, full_h: u32, region: Region) -> Image {
        self.point_region(0, &[img], &[], full_w, full_h, region)
    }

    pub fn resize_bilinear_region(&self, img: &Image, full_w: u32, full_h: u32, region: Region) -> Image {
        self.run_region(2, &[img], &[], full_w, full_h, region)
    }

    /// Identical global-grid resampling and reduction pyramid to `resize_bicubic`.
    pub fn resize_bicubic_region(&self, img: &Image, full_w: u32, full_h: u32, region: Region) -> Image {
        region.validate(full_w, full_h);
        if full_w < img.width().div_ceil(2) || full_h < img.height().div_ceil(2) {
            let half = self.resize_bicubic(img, img.width().div_ceil(2).max(full_w), img.height().div_ceil(2).max(full_h));
            return self.resize_bicubic_region(&half, full_w, full_h, region);
        }
        self.run_region(3, &[img], &[], full_w, full_h, region)
    }

    pub fn unmult_region(&self, img: &Image, full_w: u32, full_h: u32, region: Region) -> Image {
        self.point_region(22, &[img], &[], full_w, full_h, region)
    }

    pub fn quantize_region(&self, img: &Image, d: Depth, full_w: u32, full_h: u32, region: Region) -> Image {
        self.point_region(23, &[img], &[depth(d)], full_w, full_h, region)
    }

    pub fn convert_depth_region(&self, img: &Image, from: Depth, to: Depth, full_w: u32, full_h: u32, region: Region) -> Image {
        self.point_region(31, &[img], &[depth(from), depth(to)], full_w, full_h, region)
    }

    pub fn blend_region(&self, base: &Image, top: &Image, mode: BlendMode, opacity: f32, fill: f32, d: Depth, full_w: u32, full_h: u32, region: Region) -> Image {
        self.point_region(25, &[base, top], &[mode as u32 as f32, opacity, fill, depth(d)], full_w, full_h, region)
    }

    /// Resample a viewport without creating a full-size intermediate. Samples
    /// outside the image use clamped edge pixels, as the resize operations do.
    pub fn sample_view(&self, src: &Image, map: ViewMap, w: u32, h: u32, filter: Filter) -> Image {
        assert!(w > 0 && h > 0 && map.origin_x.is_finite() && map.origin_y.is_finite()
            && map.src_per_dst.is_finite() && map.src_per_dst > 0.);
        let values = [map.origin_x as f32, map.origin_y as f32, map.src_per_dst as f32];
        assert!(values.iter().all(|v| v.is_finite()) && values[2] > 0.);
        self.run(35, &[src], w, h, &[values[0], values[1], values[2], if filter == Filter::Bilinear { 1. } else { 0. }], &[])
    }
}
