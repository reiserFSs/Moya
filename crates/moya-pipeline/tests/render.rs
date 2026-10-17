use std::sync::Arc;
use moya_core::{CaChannels, CaType, GlareBlend, GlareRays, Highlights, LayerBlend, LayerInput, LayerSettings, Params, RenderMode, SpectrumBlend, Transition};
use moya_gpu::{BlendMode, Depth, Gpu, Image};
use moya_pipeline::{Document, DocumentOutput, LayerInputs, RenderRequest, ViewFlags};

fn request(mode: RenderMode, view: ViewFlags) -> RenderRequest {
    RenderRequest { mode, view, selected: Some(1), solo_layer: None, want_layer_glows: true, want_xray: true, want_composite: true, want_glow_composite: true }
}

fn single(document: &mut Document, params: &Params, mode: RenderMode, view: ViewFlags, mask: Option<&Image>, mask_generation: u64) -> DocumentOutput {
    let settings = LayerSettings { params: params.clone(), ..LayerSettings::default() };
    document.render(&[LayerInputs { settings: &settings, mask, mask_generation }], &request(mode, view))
}

#[test]
fn real_gpu_effects_caches_and_mask() {
    let gpu=Arc::new(Gpu::new_blocking().expect("headless GPU"));
    let mut pixels=vec![0.;64*48*4];
    for y in 0..48 { for x in 0..64 {
        let i=(y*64+x)*4;
        let light=if (x as i32-28).pow(2)+(y as i32-20).pow(2)<20 {1.} else {0.01};
        pixels[i..i+4].copy_from_slice(&[light,light*0.8,light*0.5,1.]);
    }}
    let mut document=Document::new(gpu.clone(),gpu.upload_rgba32f(64,48,&pixels),Depth::Eight);
    let view=ViewFlags{xray:false,solo:true,host_is_float:false};
    let mut p=Params{radius:12.,glow_iterations:4.,glare_on:true,rays_on:true,ca_on:true,tonemap:true,
        glare_length:0.1,spectrum_mix:30.,protect_core:true,..Params::default()};
    let first=single(&mut document,&p,RenderMode::Draft,view,None,0);
    let first=gpu.download_rgba32f(&first.layer_glows[0].1);
    assert!(first.iter().all(|v|v.is_finite()));
    assert!(first.chunks_exact(4).any(|v|v[0]>0.1));
    let cached=single(&mut document,&p,RenderMode::Draft,view,None,0);
    assert_eq!(first,gpu.download_rgba32f(&cached.layer_glows[0].1));
    p.threshold+=0.1;
    assert_eq!(single(&mut document,&p,RenderMode::Draft,view,None,0).xray.unwrap().width(),64);
    p.ca_type=CaType::Center; p.ca_mix=true; p.ca_blur=1.; p.hdr32=true;
    let out=single(&mut document,&p,RenderMode::Hq,view,None,0);
    assert!(gpu.download_rgba32f(&out.layer_glows[0].1).iter().all(|v|v.is_finite()));
    let mask=gpu.fill(64,48,[0.,0.,0.,1.]);
    p.hdr32=false; p.tonemap=false; p.ca_on=false; p.glare_on=false; p.rays_on=false;
    let out=single(&mut document,&p,RenderMode::Draft,view,Some(&mask),1);
    assert!(gpu.download_rgba32f(&out.layer_glows[0].1).chunks_exact(4).all(|v|v[..3].iter().all(|c|*c==0.)));
    let composite_view=ViewFlags{xray:false,solo:false,host_is_float:false};
    let hq=single(&mut document,&p,RenderMode::Hq,composite_view,Some(&mask),1);
    let composite=gpu.download_rgba32f(hq.composite.as_ref().unwrap());
    assert_eq!((hq.composite.as_ref().unwrap().width(),hq.composite.as_ref().unwrap().height()),(64,48));
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
        let mut cached=Document::new(gpu.clone(),gpu.upload_rgba32f(width,height,&pixels),host_depth);
        let mut p=Params{radius:3.,glow_iterations:2.,glare_length:0.01,rays_samples:3.,
            glare_on:true,ca_on:true,spectrum_mix:20.,..Params::default()};
        let mut mask=None;
        let mut mask_generation=0;
        for step in 0..132 {
            let value=next();
            match step%64 {
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
                20=>{mask=Some(gpu.fill(width,height,[value,value,value,1.])); mask_generation+=1;},
                21=>{mask=None; mask_generation+=1;},
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
                47=>p.glow_iterations=1.+(value*2.).floor(),
                // Exercise ignored sliders and both sides of glow's boolean gates.
                48=>{p.colorize=true; p.glare_on=true; p.hdr32=false;},
                49=>p.hue=-90.+value*180.,
                50=>p.saturation=value*100.,
                51=>p.glare_detail=0.,
                52=>p.glare_detail=0.1+value*0.1,
                53=>p.glare_detail=0.3+value*0.1,
                54=>p.glare_sharpness=0.,
                55=>p.glare_sharpness=0.1+value*0.1,
                56=>p.glare_sharpness=0.3+value*0.1,
                57=>{p.colorize=false; p.hdr32=true;},
                58=>p.saturation=value*100.,
                59=>p.glare_sharpness=0.,
                60=>p.glare_sharpness=0.1+value*0.1,
                61=>p.glare_sharpness=0.3+value*0.1,
                62=>p.glare_on=false,
                _=>{p.glare_detail=value; p.glare_sharpness=value;},
            }
            let mode=if step<128 {initial_mode} else if step%2==0 {RenderMode::Draft} else {RenderMode::Hq};
            let view=ViewFlags{xray:step%3==0,solo:step%5==0,host_is_float:step%7==0};
            let mut cold=Document::new(gpu.clone(),gpu.upload_rgba32f(width,height,&pixels),host_depth);
            let expected=single(&mut cold,&p,mode,view,mask.as_ref(),mask_generation);
            let actual=single(&mut cached,&p,mode,view,mask.as_ref(),mask_generation);
            for (name,a,b) in [("glow",&actual.layer_glows[0].1,&expected.layer_glows[0].1),("xray",actual.xray.as_ref().unwrap(),expected.xray.as_ref().unwrap()),("composite",actual.composite.as_ref().unwrap(),expected.composite.as_ref().unwrap())] {
                assert_eq!(bits(&gpu,a),bits(&gpu,b),"step {step}, {mode:?}, {host_depth:?}, {name}");
            }
            let repeated=single(&mut cached,&p,mode,view,mask.as_ref(),mask_generation);
            for (name,a,b) in [("glow",&repeated.layer_glows[0].1,&expected.layer_glows[0].1),("xray",repeated.xray.as_ref().unwrap(),expected.xray.as_ref().unwrap()),("composite",repeated.composite.as_ref().unwrap(),expected.composite.as_ref().unwrap())] {
                assert_eq!(bits(&gpu,a),bits(&gpu,b),"repeat step {step}, {name}");
            }
        }
      }
    }
}

fn stack(document: &mut Document, layers: &[LayerSettings], request: &RenderRequest) -> DocumentOutput {
    let inputs: Vec<_> = layers.iter().map(|settings| LayerInputs { settings, mask: None, mask_generation: 0 }).collect();
    document.render(&inputs, request)
}

fn layers() -> [LayerSettings; 2] {
    let params=Params { radius: 2., glow_iterations: 1., colorize: true, saturation: 100., ..Params::default() };
    [
        LayerSettings { id: 1, opacity: 0.35, params: Params { hue: -80., ..params.clone() }, ..LayerSettings::default() },
        LayerSettings { id: 2, opacity: 0.7, params: Params { hue: 90., ..params }, ..LayerSettings::default() },
    ]
}

#[test]
fn two_layers_compose_in_order_with_every_blend_opacity_visibility_and_solo() {
    let gpu=Arc::new(Gpu::new_blocking().expect("headless GPU"));
    let view=ViewFlags { xray: false, solo: false, host_is_float: false };
    for depth in [Depth::Eight, Depth::Float] {
        let source=gpu.fill(5,3,[0.2,0.4,0.7,1.]);
        let mut document=Document::new(gpu.clone(),gpu.copy(&source),depth);
        let mut layers=layers();
        let mut request=request(RenderMode::Draft,view);
        request.view.host_is_float=depth==Depth::Float;
        let composition_depth=if depth==Depth::Float {Depth::Float} else {Depth::Eight};
        for blend in [LayerBlend::Auto,LayerBlend::Screen,LayerBlend::LinearDodge,LayerBlend::Lighten,LayerBlend::Normal] {
            for layer in &mut layers {layer.blend=blend;}
            let output=stack(&mut document,&layers,&request);
            assert_eq!(output.layer_glows.iter().map(|(id,_)|*id).collect::<Vec<_>>(),[1,2]);
            let mode=match blend {
                LayerBlend::Auto if depth==Depth::Float=>BlendMode::LinearDodge,
                LayerBlend::Auto|LayerBlend::Screen=>BlendMode::Screen,
                LayerBlend::LinearDodge=>BlendMode::LinearDodge,
                LayerBlend::Lighten=>BlendMode::Lighten,
                LayerBlend::Normal=>BlendMode::Normal,
            };
            let mut expected=gpu.copy(&source);
            let mut expected_glow=gpu.fill(5,3,[0.,0.,0.,1.]);
            for (layer,(_,glow)) in layers.iter().zip(&output.layer_glows) {
                let glow=if depth==Depth::Float {gpu.convert_depth(glow,Depth::Eight,Depth::Float)} else {gpu.copy(glow)};
                expected=gpu.blend(&expected,&glow,mode,layer.opacity,1.,composition_depth);
                expected_glow=gpu.blend(&expected_glow,&glow,mode,layer.opacity,1.,composition_depth);
            }
            assert_eq!(bits(&gpu,output.composite.as_ref().unwrap()),bits(&gpu,&expected),"{blend:?}, {depth:?}");
            let expected_glow=if depth==Depth::Float {gpu.convert_depth(&expected_glow,Depth::Float,Depth::Eight)} else {expected_glow};
            assert_eq!(bits(&gpu,output.glow_composite.as_ref().unwrap()),bits(&gpu,&expected_glow),"stored glow {blend:?}, {depth:?}");
        }
        let forward=stack(&mut document,&layers,&request);
        layers.reverse();
        let reversed=stack(&mut document,&layers,&request);
        assert_ne!(bits(&gpu,forward.composite.as_ref().unwrap()),bits(&gpu,reversed.composite.as_ref().unwrap()),"Normal layers must not commute");
        layers[0].visible=false;
        let hidden=stack(&mut document,&layers,&request);
        let only=stack(&mut document,&layers[1..],&request);
        assert_eq!(bits(&gpu,hidden.composite.as_ref().unwrap()),bits(&gpu,only.composite.as_ref().unwrap()));
        layers[0].visible=true;
        layers[0].opacity=0.;
        let zero=stack(&mut document,&layers,&request);
        assert_eq!(bits(&gpu,zero.composite.as_ref().unwrap()),bits(&gpu,only.composite.as_ref().unwrap()));
        layers[0].opacity=0.7;
        request.solo_layer=Some(layers[1].id);
        let solo=stack(&mut document,&layers,&request);
        assert_eq!(bits(&gpu,solo.composite.as_ref().unwrap()),bits(&gpu,only.composite.as_ref().unwrap()));
        layers[1].visible=false;
        let hidden_solo=stack(&mut document,&layers,&request);
        assert_eq!(bits(&gpu,hidden_solo.composite.as_ref().unwrap()),bits(&gpu,&source),"solo does not reveal hidden layers");
    }
}

#[test]
fn below_extracts_from_actual_composite_and_keeps_dependencies_when_soloed() {
    let gpu=Arc::new(Gpu::new_blocking().expect("headless GPU"));
    let source=gpu.fill(7,5,[0.4,0.4,0.4,1.]);
    let mut document=Document::new(gpu.clone(),gpu.copy(&source),Depth::Eight);
    let mut layers=layers();
    // Soft extraction at gamma 1 preserves midtones; avoid a black-on-black oracle.
    for layer in &mut layers {
        layer.params=Params { radius:2.,glow_iterations:1.,threshold:1.,transition:Transition::Soft,
            dr_compression:0.,exposure:0.,gamma:1.,main_opacity:100.,core_opacity:100.,
            colorize:false,tonemap:false,protect_core:false,unmult:false,..Params::default() };
    }
    layers[1].input=LayerInput::Below;
    let view=ViewFlags { xray: false, solo: false, host_is_float: false };
    for mode in [RenderMode::Draft,RenderMode::Hq] {
        let mut request=request(mode,view);
        let below=stack(&mut document,&layers[..1],&request);
        let mut reference=Document::new(gpu.clone(),gpu.copy(below.composite.as_ref().unwrap()),Depth::Eight);
        let mut upper=layers[1].clone();
        upper.input=LayerInput::Original;
        let expected=stack(&mut reference,&[upper.clone()],&request);
        let actual=stack(&mut document,&layers,&request);
        assert_eq!(bits(&gpu,&actual.layer_glows[1].1),bits(&gpu,&expected.layer_glows[0].1),"{mode:?}");
        assert_eq!(bits(&gpu,actual.composite.as_ref().unwrap()),bits(&gpu,expected.composite.as_ref().unwrap()),"{mode:?}");
        let mut original=Document::new(gpu.clone(),gpu.copy(&source),Depth::Eight);
        let independent=stack(&mut original,&[upper],&request);
        assert_ne!(bits(&gpu,&actual.layer_glows[1].1),bits(&gpu,&independent.layer_glows[0].1),"Below must not silently use Original");
        request.solo_layer=Some(2);
        let solo=stack(&mut document,&layers,&request);
        assert_eq!(bits(&gpu,&solo.layer_glows.last().unwrap().1),bits(&gpu,&expected.layer_glows[0].1),"solo retains the below dependency");
        layers[0].visible=false;
        let hidden=stack(&mut document,&layers,&request);
        assert_eq!(bits(&gpu,&hidden.layer_glows.last().unwrap().1),bits(&gpu,&independent.layer_glows[0].1),"hidden layers do not contribute to Below");
        layers[0].visible=true;
    }
}

#[test]
fn draft_and_hq_keep_layer_shape_and_sequential_checkpoint_structure() {
    let gpu=Arc::new(Gpu::new_blocking().expect("headless GPU"));
    // A one-row document crosses Draft's width cap without a large GPU fixture.
    let mut document=Document::new(gpu.clone(),gpu.fill(1501,1,[0.2,0.4,0.7,1.]),Depth::Eight);
    let mut layers=layers();
    layers[1].input=LayerInput::Below;
    let inputs:Vec<_>=layers.iter().map(|settings|LayerInputs { settings,mask:None,mask_generation:0 }).collect();
    for (mode,width) in [(RenderMode::Draft,1500),(RenderMode::Hq,1501)] {
        let request=request(mode,ViewFlags { xray:false,solo:false,host_is_float:false });
        let mut stages=Vec::new();
        let traced=document.render_with_checkpoints(&inputs,&request,|name,_,_|stages.push(name.to_owned()));
        let sequence=["extraction","exposure_depth","merged_glow","tone","glare_composite","final_glow","ca","output"];
        assert_eq!(stages,sequence.iter().chain(sequence.iter()).map(|name|name.to_string()).collect::<Vec<_>>(),"{mode:?}: layers run sequentially");
        for image in [traced.composite.as_ref().unwrap(),traced.glow_composite.as_ref().unwrap(),&traced.layer_glows[0].1,&traced.layer_glows[1].1,traced.xray.as_ref().unwrap()] {
            assert_eq!((image.width(),image.height()),(width,1),"{mode:?}");
        }
        let cached=document.render(&inputs,&request);
        assert_eq!(bits(&gpu,traced.composite.as_ref().unwrap()),bits(&gpu,cached.composite.as_ref().unwrap()));
        document.clear_stage_caches();
        let cold=document.render(&inputs,&request);
        assert_eq!(bits(&gpu,cached.composite.as_ref().unwrap()),bits(&gpu,cold.composite.as_ref().unwrap()));
        document.clear_caches();
        let rebuilt=document.render(&inputs,&request);
        assert_eq!(bits(&gpu,cached.composite.as_ref().unwrap()),bits(&gpu,rebuilt.composite.as_ref().unwrap()));
    }
}

#[test]
fn xray_only_preview_matches_full_render_without_glow_checkpoints() {
    let gpu=Arc::new(Gpu::new_blocking().expect("headless GPU"));
    let mut document=Document::new(gpu.clone(),gpu.fill(5,3,[0.2,0.4,0.7,1.]),Depth::Eight);
    let layers=[LayerSettings { params:Params { radius:2.,glow_iterations:1.,..Params::default() },..LayerSettings::default() }];
    let inputs=[LayerInputs { settings:&layers[0],mask:None,mask_generation:0 }];
    for mode in [RenderMode::Draft,RenderMode::Hq] {
        let mut request=request(mode,ViewFlags { xray:true,solo:false,host_is_float:false });
        request.want_layer_glows=false;
        request.want_glow_composite=false;
        document.clear_caches();
        let mut stages=Vec::new();
        let preview=document.render_with_checkpoints(&inputs,&request,|name,_,_|stages.push(name.to_owned()));
        assert!(stages.is_empty(),"xray-only preview must not execute the glow graph");
        assert!(preview.layer_glows.is_empty());
        request.want_layer_glows=true;
        let full=document.render(&inputs,&request);
        assert_eq!(bits(&gpu,preview.composite.as_ref().unwrap()),bits(&gpu,full.composite.as_ref().unwrap()),"{mode:?}");
        assert_eq!(bits(&gpu,preview.xray.as_ref().unwrap()),bits(&gpu,full.xray.as_ref().unwrap()),"{mode:?}");
    }
}
