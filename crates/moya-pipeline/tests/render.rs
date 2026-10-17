use std::sync::Arc;
use moya_core::{CaChannels, CaType, GlareBlend, GlareRays, Highlights, Params, RenderMode, SpectrumBlend, Transition};
use moya_gpu::{Depth, Gpu};
use moya_pipeline::{Renderer, ViewFlags};

#[test]
fn real_gpu_effects_caches_and_mask() {
    let gpu=Arc::new(Gpu::new_blocking().expect("headless GPU"));
    let mut pixels=vec![0.;64*48*4];
    for y in 0..48 { for x in 0..64 {
        let i=(y*64+x)*4;
        let light=if (x as i32-28).pow(2)+(y as i32-20).pow(2)<20 {1.} else {0.01};
        pixels[i..i+4].copy_from_slice(&[light,light*0.8,light*0.5,1.]);
    }}
    let mut renderer=Renderer::new(gpu.clone(),gpu.upload_rgba32f(64,48,&pixels),Depth::Eight);
    let view=ViewFlags{xray:false,solo:true,host_is_float:false};
    let mut p=Params{radius:12.,glow_iterations:4.,glare_on:true,rays_on:true,ca_on:true,tonemap:true,
        glare_length:0.1,spectrum_mix:30.,protect_core:true,..Params::default()};
    let first=renderer.render(&p,RenderMode::Draft,&view);
    let first=gpu.download_rgba32f(&first.glow);
    assert!(first.iter().all(|v|v.is_finite()));
    assert!(first.chunks_exact(4).any(|v|v[0]>0.1));
    let cached=renderer.render(&p,RenderMode::Draft,&view);
    assert_eq!(first,gpu.download_rgba32f(&cached.glow));
    p.threshold+=0.1;
    assert_eq!(renderer.render(&p,RenderMode::Draft,&view).xray.width(),64);
    p.ca_type=CaType::Center; p.ca_mix=true; p.ca_blur=1.; p.hdr32=true;
    let out=renderer.render(&p,RenderMode::Hq,&view);
    assert!(gpu.download_rgba32f(&out.glow).iter().all(|v|v.is_finite()));
    renderer.set_mask(Some(gpu.fill(64,48,[0.,0.,0.,1.])));
    p.hdr32=false; p.tonemap=false; p.ca_on=false; p.glare_on=false; p.rays_on=false;
    let out=renderer.render(&p,RenderMode::Draft,&view);
    assert!(gpu.download_rgba32f(&out.glow).chunks_exact(4).all(|v|v[..3].iter().all(|c|*c==0.)));
    let composite_view=ViewFlags{xray:false,solo:false,host_is_float:false};
    let hq=renderer.render(&p,RenderMode::Hq,&composite_view);
    let composite=gpu.download_rgba32f(&hq.composite);
    assert_eq!((hq.composite.width(),hq.composite.height()),(64,48));
    // Zero mask removes glow, not the original host document under the HQ element.
    assert!(composite[0]>0.005 && composite[3]==1.);
}

fn bits(gpu:&Gpu,image:&moya_gpu::Image)->Vec<u32> {
    gpu.download_rgba32f(image).into_iter().map(f32::to_bits).collect()
}

#[test]
fn staged_cache_matches_cold_over_parameter_mask_and_view_sequences() {
    let gpu=Arc::new(Gpu::new_blocking().expect("headless GPU"));
    let (width,height)=(17,13);
    let pixels:Vec<f32>=(0..width*height).flat_map(|i| {
        let v=(i%23) as f32/22.; [v,v*0.7,(1.-v)*0.8,1.]
    }).collect();
    let mut state=0x4f4e4952u32;
    let mut next=|| { state=state.wrapping_mul(1664525).wrapping_add(1013904223); (state>>8) as f32/16777216. };
    for host_depth in [Depth::Eight,Depth::Float] {
      for initial_mode in [RenderMode::Draft,RenderMode::Hq] {
        let mut cached=Renderer::new(gpu.clone(),gpu.upload_rgba32f(width,height,&pixels),host_depth);
        let mut p=Params{radius:3.,glow_iterations:2.,glare_length:0.01,rays_samples:3.,
            glare_on:true,ca_on:true,spectrum_mix:20.,..Params::default()};
        let mut mask:Option<f32>=None;
        for step in 0..100 {
            let value=next();
            match step%48 {
                0=>p.gamma=0.8+value*1.5,
                1=>p.hue=-90.+value*180.,
                2=>p.ca_distance=value*4.,
                3=>p.glare_length=0.005+value*0.015,
                4=>p.threshold=0.5+value,
                5=>p.exposure=-0.2+value,
                6=>p.radius=1.+value*5.,
                7=>p.colorize=!p.colorize,
                8=>p.hdr32=!p.hdr32,
                9=>p.tonemap=!p.tonemap,
                10=>p.glare_detail=if value>0.5 {0.2} else {0.},
                11=>p.glare_sharpness=if value>0.5 {0.1} else {0.},
                12=>p.rays_on=!p.rays_on,
                13=>p.main_opacity=30.+value*70.,
                14=>p.core_opacity=value*100.,
                15=>p.saturation=50.+value*50.,
                16=>p.protect_core=!p.protect_core,
                17=>p.unmult=!p.unmult,
                18=>p.aspect_ratio=if value>0.5 {1.5} else {1.},
                19=>p.spectrum_frequency=1.+value*8.,
                20=>{mask=Some(value); cached.set_mask(Some(gpu.fill(width,height,[value,value,value,1.])));},
                21=>{mask=None; cached.set_mask(None);},
                22=>p.ca_type=if value>0.5 {CaType::Offset} else {CaType::Center},
                23=>p.ca_blur=value,
                24=>p.dr_compression=value*80.,
                25=>p.glare_opacity=value*100.,
                26=>p.glare_rotation=value*90.,
                27=>p.spectrum_mix=value*100.,
                28=>p.spectrum_blur=value*0.1,
                29=>p.spectrum_saturation=value*100.,
                30=>p.glare_rays=if value>0.5 {GlareRays::Four} else {GlareRays::Six},
                31=>p.glare_on=!p.glare_on,
                32=>p.glare_blend=if value>0.5 {GlareBlend::ColorDodge} else {GlareBlend::LinearDodge},
                33=>p.ca_channels=if value>0.5 {CaChannels::Bgr} else {CaChannels::MagentaGreen},
                34=>p.spectrum_blend=if value>0.5 {SpectrumBlend::SoftLight} else {SpectrumBlend::Multiply},
                35=>p.transition=if value>0.5 {Transition::Soft} else {Transition::Precise},
                36=>p.ca_direction=value*180.,
                37=>p.highlights=if value>0.5 {Highlights::Flatten} else {Highlights::Preserve},
                38=>p.ca_on=!p.ca_on,
                39=>p.ca_mix=!p.ca_mix,
                40=>p.ca_intensity=value*10.,
                41=>p.aspect_rotation=value*45.,
                42=>p.rays_center=[value,1.-value],
                43=>p.rays_spacing=value*10.,
                44=>p.rays_samples=3.+value*3.,
                45=>p.rays_falloff=0.5+value,
                46=>p.glare_gamma=0.8+value,
                _=>p.glow_iterations=1.+(value*2.).floor(),
            }
            let mode=if step<96 {initial_mode} else if step%2==0 {RenderMode::Draft} else {RenderMode::Hq};
            let view=ViewFlags{xray:step%3==0,solo:step%5==0,host_is_float:step%7==0};
            let mut cold=Renderer::new(gpu.clone(),gpu.upload_rgba32f(width,height,&pixels),host_depth);
            if let Some(v)=mask {cold.set_mask(Some(gpu.fill(width,height,[v,v,v,1.])));}
            let expected=cold.render(&p,mode,&view);
            let actual=cached.render(&p,mode,&view);
            for (name,a,b) in [("glow",&actual.glow,&expected.glow),("xray",&actual.xray,&expected.xray),("composite",&actual.composite,&expected.composite)] {
                assert_eq!(bits(&gpu,a),bits(&gpu,b),"step {step}, {mode:?}, {host_depth:?}, {name}");
            }
            assert_eq!(bits(&gpu,&cached.render_preview(&p,mode,&view)),bits(&gpu,&expected.composite),"preview step {step}");
            let repeated=cached.render(&p,mode,&view);
            for (name,a,b) in [("glow",&repeated.glow,&expected.glow),("xray",&repeated.xray,&expected.xray),("composite",&repeated.composite,&expected.composite)] {
                assert_eq!(bits(&gpu,a),bits(&gpu,b),"repeat step {step}, {name}");
            }
        }
      }
    }
}
