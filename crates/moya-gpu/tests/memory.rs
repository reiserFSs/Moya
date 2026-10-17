use moya_gpu::{Depth, Gpu};

#[test]
fn idle_trim_is_lru_and_clones_are_one_live_texture() {
    let gpu = Gpu::new_blocking().unwrap();
    let a = gpu.fill(2, 3, [0.25; 4]);
    let alias = a.clone();
    assert_eq!(a.storage_id(), alias.storage_id());
    let b = gpu.fill(3, 3, [0.5; 4]);
    assert_eq!(gpu.memory_stats().live_bytes, (6 + 9) * 16);
    assert_eq!(gpu.memory_stats().live_count, 2);
    drop(a);
    assert_eq!(gpu.memory_stats().live_count, 2);
    drop(alias);
    drop(b);
    assert_eq!(gpu.memory_stats().idle_bytes, (6 + 9) * 16);
    gpu.trim_idle(9 * 16);
    let stats = gpu.memory_stats();
    assert_eq!((stats.live_count, stats.live_bytes, stats.idle_count, stats.idle_bytes), (0, 0, 1, 9 * 16));
    let b = gpu.fill(3, 3, [0.75; 4]);
    assert_eq!(gpu.memory_stats().idle_count, 0, "newer size must survive trim");
    gpu.trim_idle(0);
    assert_eq!(gpu.memory_stats().live_count, 1);
    drop(b);
    gpu.trim_idle(0);
    assert_eq!(gpu.memory_stats().idle_bytes, 0);
}

#[test]
fn rounding_owned_alias_does_not_mutate_shared_output() {
    let gpu = Gpu::new_blocking().unwrap();
    let original = gpu.fill(1, 1, [0.1234567, 0.2345678, 0.3456789, 1.0]);
    let alias = original.clone();
    let rounded = gpu.quantize_owned(original, Depth::Eight);
    assert_eq!(gpu.download_rgba32f(&alias), [0.1234567, 0.2345678, 0.3456789, 1.0]);
    assert_ne!(gpu.download_rgba32f(&rounded)[0], gpu.download_rgba32f(&alias)[0]);
}

#[test]
fn bounded_rows_match_full_readback_and_propagate_errors() {
    let gpu = Gpu::new_blocking().unwrap();
    for (w, h) in [(1, 1), (17, 3), (19, 517)] {
        let pixels: Vec<f32> = (0..w * h * 4).map(|i| f32::from_bits(0x3e000000 + i)).collect();
        let img = gpu.upload_rgba32f(w, h, &pixels);
        for band in [1, 2, 256, 1000] {
            let mut streamed = Vec::new();
            let mut next = 0;
            gpu.download_rows(&img, band, |y, rows, data| {
                assert_eq!(y, next);
                assert!(rows <= band.min(256));
                assert_eq!(data.len(), w as usize * rows as usize * 4);
                next += rows;
                streamed.extend_from_slice(data);
                Ok(())
            }).unwrap();
            assert_eq!(streamed.iter().map(|p| p.to_bits()).collect::<Vec<_>>(), pixels.iter().map(|p| p.to_bits()).collect::<Vec<_>>());
            assert_eq!(next, h);
        }
        let mut pulled = vec![0.0; pixels.len()];
        gpu.download_row_range(&img, 0, h, &mut pulled).unwrap();
        assert_eq!(pulled, pixels);
        assert!(gpu.download_rows(&img, 0, |_, _, _| Ok(())).is_err());
        assert!(gpu.download_rows(&img, 1, |_, _, _| anyhow::bail!("stop")).is_err());
        assert!(gpu.download_row_range(&img, h, 1, &mut [0.0; 4]).is_err());
    }
}

#[test]
fn every_u16_mask_code_roundtrips_exactly() {
    let gpu = Gpu::new_blocking().unwrap();
    let codes: Vec<u16> = (0..=u16::MAX).collect();
    let img = gpu.upload_gray_u16(128, 512, &codes);
    assert_eq!(gpu.download_gray_u16(&img), codes);
    gpu.download_rows(&img, 31, |y, _, rgba| {
        for (i, p) in rgba.chunks_exact(4).enumerate() {
            let v = codes[y as usize * 128 + i] as f32 / 65535.0;
            assert!(p[..3].iter().all(|x| x.to_bits() == v.to_bits()));
            assert_eq!(p[3].to_bits(), 1.0f32.to_bits());
        }
        Ok(())
    }).unwrap();
}

#[test]
fn peak_counts_live_textures_and_bounded_readback_buffer() {
    let gpu = Gpu::new_blocking().unwrap();
    let pixels = vec![0.25; 17 * 300 * 4];
    let image = gpu.upload_rgba32f(17, 300, &pixels);
    gpu.reset_peak();
    let live = 17 * 300 * 16;
    assert_eq!(gpu.memory_stats().peak_live_bytes, live);
    gpu.download_rows(&image, 7, |_, _, _| Ok(())).unwrap();
    assert_eq!(gpu.memory_stats().peak_live_bytes, live + 512 * 7);
    gpu.reset_peak();
    assert_eq!(gpu.memory_stats().peak_live_bytes, live);
    drop(image);
    gpu.reset_peak();
    assert_eq!(gpu.memory_stats().peak_live_bytes, 0);
}

#[test]
fn peak_includes_temporary_gray_upload_texture() {
    let gpu = Gpu::new_blocking().unwrap();
    gpu.reset_peak();
    let image = gpu.upload_gray32f(17, 19, &[0.5; 17 * 19]);
    assert_eq!(gpu.memory_stats().peak_live_bytes, 17 * 19 * (16 + 4));
    gpu.download_rows(&image, 1, |_, _, _| Ok(())).unwrap();
    gpu.reset_peak();
    assert_eq!(gpu.memory_stats().peak_live_bytes, 17 * 19 * 16);
}
