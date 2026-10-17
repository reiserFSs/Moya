use moya_gpu::{BlendMode, Depth, Filter, Gpu, Image, Region, ViewMap};

fn pixels(w: u32, h: u32, mut seed: u32) -> Vec<f32> {
    (0..w*h*4).map(|i| {
        seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5;
        if i % 4 == 3 { (seed % 1025) as f32 / 1024. } else { (seed % 2049) as f32 / 1024. - 0.25 }
    }).collect()
}
fn regions(w: u32, h: u32) -> Vec<Region> {
    let mut result = Vec::new();
    for (x,y) in [(0,0),(w-1,0),(0,h-1),(w-1,h-1),(w/2,h/2)] {
        result.push(Region{x,y,w:1,h:1});
    }
    for (x,y) in [(0,0),(w-3,0),(0,h-2),(w-3,h-2),(w/2-1,h/2)] {
        result.push(Region{x,y,w:3,h:2});
    }
    result.push(Region{x:0,y:0,w,h});
    result
}
fn crop(values: &[f32], w: u32, r: Region) -> Vec<f32> {
    (r.y..r.y+r.h).flat_map(|y| {
        let start = ((y*w+r.x)*4) as usize;
        values[start..start+(r.w*4) as usize].iter().copied()
    }).collect()
}
fn equal(g: &Gpu, full: &Image, region: &Image, r: Region) {
    assert_eq!((region.width(),region.height()),(r.w,r.h));
    let expected = crop(&g.download_rgba32f(full),full.width(),r);
    let actual = g.download_rgba32f(region);
    assert_eq!(actual.iter().map(|v|v.to_bits()).collect::<Vec<_>>(),
        expected.iter().map(|v|v.to_bits()).collect::<Vec<_>>(), "region {r:?}");
}

#[test]
fn every_finishing_region_matches_full_operation_bits() {
    let g = Gpu::new_blocking().expect("mandatory GPU");
    for (w,h) in [(7,5),(19,13)] {
        let a = g.upload_rgba32f(w,h,&pixels(w,h,17));
        let b = g.upload_rgba32f(w,h,&pixels(w,h,123));
        for r in regions(w,h) {
            let ca = g.copy_region(&a,w,h,r);
            let cb = g.copy_region(&b,w,h,r);
            equal(&g,&a,&ca,r);
            equal(&g,&g.unmult(&a),&g.unmult_region(&a,w,h,r),r);
            equal(&g,&g.unmult(&a),&g.unmult_region(&ca,w,h,r),r);
            for d in [Depth::Eight,Depth::Sixteen,Depth::Float] {
                let q = g.quantize(&a,d);
                equal(&g,&q,&g.quantize_region(&a,d,w,h,r),r);
                equal(&g,&q,&g.quantize_region(&ca,d,w,h,r),r);
                for from in [Depth::Eight,Depth::Sixteen,Depth::Float] {
                    let full = g.convert_depth(&a,from,d);
                    equal(&g,&full,&g.convert_depth_region(&a,from,d,w,h,r),r);
                    equal(&g,&full,&g.convert_depth_region(&ca,from,d,w,h,r),r);
                }
                for mode in [BlendMode::Normal,BlendMode::Screen,BlendMode::LinearDodge,BlendMode::Lighten] {
                    for opacity in [0.,0.37,1.] {
                        let full = g.blend(&a,&b,mode,opacity,0.63,d);
                        for (base,top) in [(&a,&b),(&ca,&cb),(&a,&cb),(&ca,&b)] {
                            equal(&g,&full,&g.blend_region(base,top,mode,opacity,0.63,d,w,h,r),r);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn resize_regions_match_full_grid_at_edges_and_reduction_pyramids() {
    let g = Gpu::new_blocking().expect("mandatory GPU");
    for (sw,sh,w,h) in [(5,3,19,13),(19,13,7,5),(31,23,5,3),(7,5,7,5),(3,17,13,5)] {
        let src = g.upload_rgba32f(sw,sh,&pixels(sw,sh,43));
        let cubic = g.resize_bicubic(&src,w,h);
        let linear = g.resize_bilinear(&src,w,h);
        for r in regions(w,h) {
            equal(&g,&cubic,&g.resize_bicubic_region(&src,w,h,r),r);
            equal(&g,&linear,&g.resize_bilinear_region(&src,w,h,r),r);
        }
    }
}

#[test]
fn sample_view_uses_pixel_centers_filters_and_clamped_edges() {
    let g = Gpu::new_blocking().expect("mandatory GPU");
    let (w,h)=(7,5);
    let values = pixels(w,h,98);
    let src = g.upload_rgba32f(w,h,&values);
    let get = |x:i32,y:i32,k:usize| values[((y.clamp(0,h as i32-1) as u32*w+x.clamp(0,w as i32-1) as u32)*4) as usize+k];
    for map in [ViewMap{origin_x:0.,origin_y:0.,src_per_dst:1.},
        ViewMap{origin_x:1.,origin_y:2.,src_per_dst:1.},
        ViewMap{origin_x:-1.25,origin_y:-0.75,src_per_dst:0.5},
        ViewMap{origin_x:4.25,origin_y:3.5,src_per_dst:1.75}] {
        for filter in [Filter::Nearest,Filter::Bilinear] {
            let image = g.sample_view(&src,map,9,7,filter);
            let actual = g.download_rgba32f(&image);
            for y in 0..7 { for x in 0..9 { for k in 0..4 {
                let qx=map.origin_x as f32+(x as f32+0.5)*map.src_per_dst as f32-0.5;
                let qy=map.origin_y as f32+(y as f32+0.5)*map.src_per_dst as f32-0.5;
                let expected = if filter==Filter::Nearest {get((qx+0.5).floor() as i32,(qy+0.5).floor() as i32,k)} else {
                    let (ix,iy)=(qx.floor() as i32,qy.floor() as i32);
                    let (fx,fy)=(qx-qx.floor(),qy-qy.floor());
                    let mix=|a:f32,b:f32,t:f32|a*(1.-t)+b*t;
                    mix(mix(get(ix,iy,k),get(ix+1,iy,k),fx),mix(get(ix,iy+1,k),get(ix+1,iy+1,k),fx),fy)
                };
                let a=actual[((y*9+x)*4) as usize+k];
                if filter==Filter::Nearest {assert_eq!(a.to_bits(),expected.to_bits());}
                else {assert!((a-expected).abs()<1e-6,"{map:?} ({x},{y}): {a} != {expected}");}
            }}}
        }
    }
}
