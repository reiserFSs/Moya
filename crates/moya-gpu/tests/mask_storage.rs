//! Resident masks are single-channel (2 or 4 B/px, not 16) and read exactly like the RGBA32F
//! `upload_gray_u16` images they replace. A GPU adapter is mandatory, as in the numeric suite.
use moya_gpu::Gpu;

fn codes(w: u32, h: u32) -> Vec<u16> {
    let mut state = 0x9e3779b9u32;
    let mut v: Vec<u16> = (0..w * h).map(|_| { state = state.wrapping_mul(1664525).wrapping_add(1013904223); (state >> 16) as u16 }).collect();
    v[..4].copy_from_slice(&[0, 1, 32768, 65535]);
    v
}

fn bits(v: &[f32]) -> Vec<u32> { v.iter().map(|x| x.to_bits()).collect() }

#[test]
fn mask_images_use_a_fraction_of_rgba_memory() {
    let gpu = Gpu::new_blocking().expect("GPU is mandatory");
    let (w, h) = (64, 48);
    let px = u64::from(w * h);
    let c = codes(w, h);
    let rgba = gpu.upload_gray_u16(w, h, &c);
    assert_eq!(gpu.memory_stats().live_bytes, px * 16);
    drop(rgba);
    gpu.trim_idle(0);
    let before = gpu.memory_stats();
    assert_eq!((before.live_bytes, before.idle_bytes), (0, 0));
    gpu.reset_peak();
    let masks: Vec<_> = (0..8).map(|_| gpu.upload_mask_u16(w, h, &c)).collect();
    let stats = gpu.memory_stats();
    let per_mask = stats.live_bytes / 8;
    eprintln!("mask storage: {} B/px (RGBA32F: 16)", per_mask / px);
    assert!(per_mask == px * 2 || per_mask == px * 4, "mask costs {per_mask} bytes for {px} px");
    assert_eq!(stats.live_bytes, per_mask * 8);
    assert_eq!(stats.peak_live_bytes, stats.live_bytes, "uploading must not allocate RGBA temporaries");
    let zero = gpu.mask_zeros(w, h);
    assert_eq!(gpu.memory_stats().live_bytes, per_mask * 9);
    drop(masks);
    drop(zero);
    assert_eq!(gpu.memory_stats().live_bytes, 0);
    assert_eq!(gpu.memory_stats().idle_bytes, per_mask * 9, "idle pool accounts the real byte size");
    // A mask texture is never handed out as an RGBA image or vice versa.
    let rgba = gpu.fill(w, h, [0.5; 4]);
    assert_eq!(gpu.memory_stats().idle_bytes, per_mask * 9);
    assert_eq!(gpu.memory_stats().live_bytes, px * 16);
    drop(rgba);
}

#[test]
fn mask_images_read_bit_identically_to_rgba_masks() {
    let gpu = Gpu::new_blocking().expect("GPU is mandatory");
    let (w, h) = (37, 23);
    let c = codes(w, h);
    let old = gpu.upload_gray_u16(w, h, &c);
    let new = gpu.upload_mask_u16(w, h, &c);
    assert_eq!(gpu.download_gray_u16(&new), c);
    let red = |img: &moya_gpu::Image| bits(&gpu.download_rgba32f(&gpu.extract_channel(img, 0)));
    assert_eq!(red(&new), red(&old));
    assert_eq!(red(&gpu.copy(&new)), red(&gpu.copy(&old)));
    for (rw, rh) in [(19, 11), (60, 50), (37, 23), (1, 1)] {
        assert_eq!(red(&gpu.resize_bilinear(&new, rw, rh)), red(&gpu.resize_bilinear(&old, rw, rh)), "resize {rw}x{rh}");
    }
    let a = gpu.upload_rgba32f(w, h, &(0..w * h * 4).map(|i| (i % 97) as f32 / 31.0 - 0.5).collect::<Vec<_>>());
    let b = gpu.upload_rgba32f(w, h, &(0..w * h * 4).map(|i| (i % 53) as f32 / 17.0).collect::<Vec<_>>());
    assert_eq!(bits(&gpu.download_rgba32f(&gpu.mix_by_mask(&a, &b, &new))), bits(&gpu.download_rgba32f(&gpu.mix_by_mask(&a, &b, &old))));
    assert_eq!(bits(&gpu.download_rgba32f(&gpu.mask_alpha(&a, &new))), bits(&gpu.download_rgba32f(&gpu.mask_alpha(&a, &old))));
}

#[test]
fn mask_rect_writes_match_a_full_upload() {
    let gpu = Gpu::new_blocking().expect("GPU is mandatory");
    let (w, h) = (37, 23);
    let mut c = codes(w, h);
    let image = gpu.mask_zeros(w, h);
    let mut expected = vec![0u16; (w * h) as usize];
    for (x0, y0, rw, rh) in [(3u32, 5u32, 9u32, 4u32), (0, 0, w, h), (30, 20, 7, 3)] {
        let mut patch = Vec::new();
        for y in y0..y0 + rh { patch.extend_from_slice(&c[(y * w + x0) as usize..(y * w + x0 + rw) as usize]); }
        gpu.write_mask_region(&image, x0, y0, rw, rh, &patch);
        for y in y0..y0 + rh { for x in x0..x0 + rw { expected[(y * w + x) as usize] = c[(y * w + x) as usize]; } }
        assert_eq!(gpu.download_gray_u16(&image), expected);
        for v in &mut c { *v = v.wrapping_mul(31).wrapping_add(7); }
    }
}
