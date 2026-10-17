use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::{fs, path::Path};

const DEPTHS: [u8; 3] = [8, 16, 32];
const PATTERNS: [&str; 15] = [
    "impulse", "offcenter", "edge-h", "edge-v", "edge-diagonal",
    "ramp256", "ramp4096", "hsv", "rgbcube", "natural", "hdr", "gray50",
    "ripple-coordinates256", "ripple-coordinates512",
    "sharpen-midtone",
];
const RADII: [f64; 10] = [0.5, 1., 2., 5., 10., 25., 50., 100., 250., 1000.];
const ANGLES: [i32; 5] = [0, 30, 45, 90, -60];
const DISTANCES: [i32; 6] = [1, 5, 20, 100, 500, 2000];
const OPACITIES: [i32; 3] = [20, 60, 100];

fn unit(name: &str, value: f64) -> Value {
    json!({"_unit": name, "_value": value})
}

fn enumeration(name: &str, value: &str) -> Value {
    json!({"_enum": name, "_value": value})
}

fn input(pattern: &str, depth: u8) -> String {
    format!("inputs/{pattern}-{depth}.tif")
}

fn add(cases: &mut Vec<Value>, op: &str, args: Value, depth: u8, pattern: &str) {
    let id = format!("{op}-{depth}-{pattern}-{:05}", cases.len());
    cases.push(json!({
        "id": id, "op": op, "args": args, "depth": depth,
        "input": input(pattern, depth), "output": format!("ps/{id}.tif")
    }));
}

fn gradient(stops: &[(f64, [u8; 3])]) -> Value {
    let colors: Vec<_> = stops.iter().map(|(location, rgb)| json!({
        "_obj":"colorStop", "color":{"_obj":"RGBColor", "red":rgb[0], "grain":rgb[1], "blue":rgb[2]},
        "type":enumeration("colorStopType", "userStop"), "location":location * 40.96, "midpoint":50
    })).collect();
    json!({
        "gradientsInterpolationMethod":enumeration("gradientInterpolationMethodType", "perceptual"),
        "gradient":{
            "_obj":"gradientClassEvent", "name":"Custom", "gradientForm":enumeration("gradientForm", "customStops"),
            "interfaceIconFrameDimmed":4096, "colors":colors,
            "transparency":[
                {"_obj":"transferSpec", "opacity":unit("percentUnit",100.), "location":0, "midpoint":50},
                {"_obj":"transferSpec", "opacity":unit("percentUnit",100.), "location":4096, "midpoint":50}
            ]
        }
    })
}

fn manifest() -> Value {
    let mut cases = Vec::new();
    for depth in DEPTHS {
        for pattern in ["impulse"] {
            for radius in RADII {
                add(&mut cases, "gaussianBlur", json!({"radius":unit("pixelsUnit", radius)}), depth, pattern);
            }
            for angle in ANGLES {
                for distance in DISTANCES {
                    add(&mut cases, "motionBlur", json!({"angle":angle,"distance":unit("pixelsUnit",distance as f64)}), depth, pattern);
                }
            }
            for amount in [1, 10, 50, 100] {
                for quality in ["draft", "good", "best"] {
                    for (space, x, y) in [("normalized",0.5,0.5),("normalized",0.25,0.75),("pixels",128.,128.),("pixels",64.,192.)] {
                        add(&mut cases, "radialBlur", json!({
                            "amount":amount,"blurMethod":enumeration("blurMethod","zoom"),
                            "blurQuality":enumeration("blurQuality",quality),
                            "center":{"_obj":"paint","horizontal":x,"vertical":y}, "centerSpace":space
                        }), depth, pattern);
                    }
                }
            }
            for amount in [1, 100, 500, 999] {
                add(&mut cases, "ripple", json!({"amount":amount,"rippleSize":enumeration("rippleSize","large")}), depth, pattern);
            }
            for sharp in [0.1, 1., 5., 10.] {
                add(&mut cases, "smartSharpen", json!({
                    "presetKind":enumeration("presetKindType","presetKindCustom"), "useLegacy":false,
                    "amount":unit("percentUnit",500. * sharp),"radius":unit("pixelsUnit",sharp * 256. / 1300.),
                    "noiseReduction":unit("percentUnit",0.), "blur":enumeration("blurType","lensBlur"),
                    "shadowMode":{"_obj":"adaptCorrectTones","amount":unit("percentUnit",100.),"width":unit("percentUnit",52.),"radius":1}
                }), depth, pattern);
            }
            for (x, y) in [(0,0),(1,0),(0,1),(-1,-1),(17,-31),(-257,513)] {
                add(&mut cases, "offset", json!({"horizontal":x,"vertical":y,"fill":enumeration("fillMode","repeat")}), depth, pattern);
            }
        }
        for pattern in ["ramp256"] {
            for black in [0, 16] {
                for white in [235, 255] {
                    for gamma in [0.45, 1., 2.2] {
                        for output in [[0,255],[16,235]] {
                            add(&mut cases,"levels",json!({
                                "presetKind":enumeration("presetKindType","presetKindCustom"),
                                "adjustment":[{"_obj":"levelsAdjustment","channel":{"_ref":"channel","_enum":"channel","_value":"composite"},
                                    "input":[black,white],"gamma":gamma,"output":output}]
                            }),depth,pattern);
                        }
                    }
                }
            }
            for exposure in [-3., 0., 3.] {
                for gamma in [0.45, 1., 2.2] {
                    add(&mut cases,"exposure",json!({"exposure":exposure,"offset":0,"gammaCorrection":gamma}),depth,pattern);
                }
            }
            for hue in [-90, 0, 90] {
                for saturation in [-100, 0, 100] {
                    for lightness in [-50, 0, 50] {
                        add(&mut cases,"hueSaturation",json!({"colorize":false,
                            "adjustment":[{"_obj":"hueSatAdjustmentV2","hue":hue,"saturation":saturation,"lightness":lightness}]
                        }),depth,"hsv");
                    }
                }
            }
            for hue in [0, 120, 240, 360] {
                for saturation in [20, 100] {
                    add(&mut cases,"hueSaturation",json!({"colorize":true,
                        "adjustment":[{"_obj":"hueSatAdjustmentV2","hue":hue,"saturation":saturation,"lightness":0}]
                    }),depth,"hsv");
                }
            }
            for vibrance in [-100, 0, 100] {
                for saturation in [-100, 0, 100] {
                    add(&mut cases,"vibrance",json!({"vibrance":vibrance,"saturation":saturation}),depth,"hsv");
                }
            }
            add(&mut cases,"desaturate",json!({}),depth,"hsv");
            add(&mut cases,"blackAndWhite",json!({
                "presetKind":enumeration("presetKindType","presetKindDefault"),
                "red":40,"yellow":60,"grain":40,"cyan":60,"blue":20,"magenta":80,"useTint":false,
                "tintColor":{"_obj":"RGBColor","red":225.00045776367188,"grain":211.00067138671875,"blue":179.00115966796875}
            }),depth,"hsv");
            add(&mut cases,"addNoise",json!({"distort":enumeration("distort","gaussianDistribution"),
                "noise":unit("percentUnit",0.3),"monochromatic":false}),depth,pattern);
            let xray = [(0.,[0,0,0]),(1.,[0,0,118]),(2.,[0,30,150]),(3.,[0,255,255]),
                (5.,[100,255,0]),(8.,[255,255,0]),(10.,[255,0,0])];
            add(&mut cases,"gradientMapEvent",gradient(&xray),depth,pattern);
            for frequency in [1, 4, 10] {
                let period = 11 - frequency;
                let mut stops = Vec::new();
                let mut t = 0;
                while (t as f64) < 100. / period as f64 {
                    let location = period * (t + 1);
                    let location = if location > 100 { location - 100 } else { location };
                    let mut rgb = [0;3];
                    rgb[t as usize % 3] = 255;
                    stops.push((location as f64,rgb));
                    t += 1;
                }
                add(&mut cases,"gradientMapEvent",gradient(&stops),depth,pattern);
            }
            add(&mut cases,"Adobe Camera Raw Filter",json!({"$PrVN":6,"$PrVe":251920384,"$Hi12":-100}),depth,"natural");
            for target in DEPTHS {
                if target != depth {
                    add(&mut cases,"convertMode",json!({"depth":target}),depth,pattern);
                    cases.last_mut().unwrap()["depth"] = json!(target);
                }
            }
        }
        for pattern in ["offcenter", "edge-h", "edge-v", "edge-diagonal", "natural"] {
            add(&mut cases,"gaussianBlur",json!({"radius":unit("pixelsUnit",5.)}),depth,pattern);
            add(&mut cases,"motionBlur",json!({"angle":30,"distance":unit("pixelsUnit",20.)}),depth,pattern);
        }
        add(&mut cases,"radialBlur",json!({
            "amount":10,"blurMethod":enumeration("blurMethod","zoom"),
            "blurQuality":enumeration("blurQuality","good"),
            "center":{"_obj":"paint","horizontal":0.25,"vertical":0.75},"centerSpace":"normalized"
        }),depth,"offcenter");
        for gamma in [0.45,1.,2.2] {
            add(&mut cases,"levels",json!({"adjustment":[{
                "_obj":"levelsAdjustment","channel":{"_ref":"channel","_enum":"channel","_value":"composite"},
                "input":[16,235],"gamma":gamma,"output":[0,255]
            }]}),depth,"ramp4096");
        }
        // Source and base differ in hue, luminance and spatial structure.
        for (base, source) in [("rgbcube","hsv")] {
            for mode in ["screen","linearDodge","colorDodge","multiply","color"] {
                for fill in [100,60,20] {
                    for opacity in OPACITIES {
                        add(&mut cases,"layerComposite",json!({"mode":mode,"opacity":opacity,"fill":fill}),depth,base);
                        cases.last_mut().unwrap()["top"] = json!({"input":input(source,depth),"blend":mode,"opacity":opacity,"fill":fill});
                    }
                }
            }
            for mode in ["normal","lighten","multiply","divide","overlay","softLight"] {
                for opacity in OPACITIES {
                    for invert in [false,true] {
                        add(&mut cases,"applyImageEvent",json!({"calculation":mode,"opacity":opacity,"invert":invert,"scale":1,"offset":0}),depth,base);
                        cases.last_mut().unwrap()["top"] = json!({"input":input(source,depth),"blend":"normal","opacity":100,"fill":100});
                    }
                }
            }
        }
        // Gray sources exercise the singular saturation branch of Color blending.
        for (base, source) in [("hsv","gray50"),("gray50","hsv"),("gray50","gray50")] {
            add(&mut cases,"layerComposite",json!({"mode":"color","opacity":100,"fill":100}),depth,base);
            cases.last_mut().unwrap()["top"] = json!({"input":input(source,depth),"blend":"color","opacity":100,"fill":100});
        }
        if depth == 32 {
            add(&mut cases,"gaussianBlur",json!({"radius":unit("pixelsUnit",5.)}),depth,"hdr");
            for exposure in [-3.,0.,3.] { for gamma in [0.45,1.,2.2] {
                add(&mut cases,"exposure",json!({"exposure":exposure,"offset":0,"gammaCorrection":gamma}),depth,"hdr");
            }}
            for target in [8,16] {
                add(&mut cases,"convertMode",json!({"depth":target}),depth,"hdr");
                cases.last_mut().unwrap()["depth"] = json!(target);
            }
            for mode in ["screen","linearDodge","colorDodge","multiply","color"] {
                for fill in [100,60,20] {
                    for opacity in OPACITIES {
                        add(&mut cases,"layerComposite",json!({"mode":mode,"opacity":opacity,"fill":fill}),depth,"hdr");
                        cases.last_mut().unwrap()["top"] = json!({"input":input("natural",depth),"blend":mode,"opacity":opacity,"fill":fill});
                    }
                }
            }
        }
    }
    for depth in DEPTHS {
        for blend in ["colorDodge", "screen", "color"] {
            for fill in [20, 60] {
                add(&mut cases,"copyToLayer",json!({}),depth,"rgbcube");
                cases.last_mut().unwrap()["top"] = json!({
                    "input":input("hsv",depth),"blend":blend,"opacity":60,"fill":fill
                });
            }
        }
    }
    for depth in DEPTHS {
        for pattern in ["coordinates256", "coordinates512", "natural"] {
            let amounts: &[i32] = if pattern == "natural" { &[100, 500] } else { &[1, 100, 500, 999] };
            for &amount in amounts {
                let id = format!("ripple-{depth}-{pattern}-{amount}");
                let source = if pattern == "natural" { pattern.to_owned() } else { format!("ripple-{pattern}") };
                cases.push(json!({
                    "id":id, "op":"ripple", "depth":depth, "input":input(&source,depth),
                    "output":format!("ps/{id}.tif"),
                    "args":{"amount":amount,"rippleSize":enumeration("rippleSize","large")}
                }));
            }
        }
    }
    let mut repeat = cases.iter().find(|c| c["id"] == "ripple-8-coordinates256-100").unwrap().clone();
    repeat["id"] = json!("ripple-8-coordinates256-100-repeat");
    repeat["output"] = json!("ps/ripple-8-coordinates256-100-repeat.tif");
    cases.push(repeat);
    // Clone the original descriptors and suffixes so generated IDs match the captures.
    let sharpen: Vec<_> = cases.iter().filter(|c| c["op"] == "smartSharpen").cloned().collect();
    for base in sharpen {
        let depth = base["depth"].as_u64().unwrap() as u8;
        for pattern in ["natural", "sharpen-midtone"] {
            let mut case = base.clone();
            let id = base["id"].as_str().unwrap().replace("-impulse-", &format!("-{pattern}-"));
            case["id"] = json!(id);
            case["input"] = json!(input(pattern, depth));
            case["output"] = json!(format!("ps/{id}.tif"));
            cases.push(case);
        }
    }
    for depth in DEPTHS {
        for pattern in ["natural-bright", "natural-dark", "ramp256"] {
            let id = format!("ACR-{depth}-{pattern}-stylize");
            cases.push(json!({
                "id":id, "op":"Adobe Camera Raw Filter", "depth":depth,
                "input":input(pattern,depth), "output":format!("ps/{id}.tif"),
                "args":{"$PrVN":6,"$PrVe":251920384,"$Hi12":-100}
            }));
        }
    }
    json!({"schemaVersion":1,"photoshopVersion":"uncaptured","cases":cases})
}

fn hsv(h: f32, s: f32, v: f32) -> [f32; 3] {
    let sector = h * 6.;
    let c = v * s;
    let x = c * (1. - (sector % 2. - 1.).abs());
    let m = v - c;
    let rgb = match sector as u32 % 6 {
        0 => [c,x,0.], 1 => [x,c,0.], 2 => [0.,c,x],
        3 => [0.,x,c], 4 => [x,0.,c], _ => [c,0.,x],
    };
    rgb.map(|channel| channel + m)
}

fn sharpen_midtone(x: u32, y: u32) -> f64 {
    0.35 + if x >= 128 { 0.2 } else { 0. }
        + 0.035 * (f64::from(x) * std::f64::consts::TAU / 13.).sin()
            * (f64::from(y) * std::f64::consts::TAU / 17.).cos()
        + if (40..73).contains(&x) && (112..145).contains(&y) { 0.08 } else { 0. }
}

fn pattern(name: &str) -> (u32, u32, Vec<[f32; 4]>) {
    let (width, height) = match name {
        "ramp4096" => (4096,16),
        "ripple-coordinates512" => (512,512),
        _ => (256,256),
    };
    let mut pixels = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        for x in 0..width {
            let u = x as f32 / (width - 1) as f32;
            let v = y as f32 / (height - 1) as f32;
            let gray = |value| [value;3];
            let rgb = match name {
                "impulse" => gray(if x == 128 && y == 128 { 1. } else { 0. }),
                "offcenter" => gray(if x == 29 && y == 197 { 1. } else { 0. }),
                "edge-h" => gray(if y >= 128 { 1. } else { 0. }),
                "edge-v" => gray(if x >= 128 { 1. } else { 0. }),
                "edge-diagonal" => gray(if x + y >= 255 { 1. } else { 0. }),
                "ramp256" | "ramp4096" => gray(u),
                "ripple-coordinates256" | "ripple-coordinates512" => [u,v,0.5],
                "gray50" => gray(0.5),
                "sharpen-midtone" => gray(sharpen_midtone(x,y) as f32),
                "hsv" => hsv(u, (v * 4.).fract(), 0.25 + 0.25 * (v * 4.).floor().min(3.)),
                "rgbcube" => [((x % 16) as f32)/15., ((y % 16) as f32)/15., ((x / 16 + 16 * (y / 16)) as f32)/255.],
                "hdr" => [16. * u, 16. * v, 16. * u * v],
                "natural" => {
                    // Fixed analytic landscape: sky, rolling terrain, fine texture and bright sun.
                    let horizon = 0.52 + 0.08 * (u * 13.).sin() + 0.04 * (u * 37.).sin();
                    let texture = 0.025 * ((x * 73 + y * 151 + x * y * 17) % 101) as f32 / 100.;
                    let sun = (-((u-0.73).powi(2)+(v-0.22).powi(2))/0.001).exp();
                    let base = if v < horizon { [0.12+0.3*v,0.28+0.4*v,0.6+0.3*v] }
                        else { [0.16+0.2*u,0.22+0.2*(1.-v),0.06+0.08*u] };
                    base.map(|c| (c + texture + 0.8 * sun).min(1.))
                }
                _ => unreachable!("unknown pattern"),
            };
            pixels.push([rgb[0],rgb[1],rgb[2],1.]);
        }
    }
    (width,height,pixels)
}

pub fn generate(root: &Path) -> Result<()> {
    fs::create_dir_all(root.join("inputs"))?;
    fs::create_dir_all(root.join("ps"))?;
    for name in PATTERNS {
        let (width,height,pixels) = pattern(name);
        for depth in DEPTHS {
            // HDR samples remain floating point; integer encodings cannot represent 0..16.
            if name == "hdr" && depth != 32 { continue; }
            // The captured midtone input quantized to Photoshop's 32769 levels before TIFF encoding.
            let quantized;
            let pixels = if name == "sharpen-midtone" && depth != 32 {
                quantized = (0..height).flat_map(|y| (0..width).map(move |x| {
                    let value = sharpen_midtone(x,y);
                    let scale = if depth == 8 { 255. } else { 32768. };
                    let value = ((value * scale).round() / scale) as f32;
                    [value,value,value,1.]
                })).collect::<Vec<_>>();
                &quantized
            } else { &pixels };
            crate::io::save(&root.join(input(name,depth)),width,height,pixels,depth)
                .with_context(|| format!("writing {name} at {depth} bits"))?;
        }
    }
    let (width,height,base) = pattern("natural");
    for depth in DEPTHS {
        let scale = match depth { 8 => 255., 16 => 65535., _ => 1. };
        for name in ["natural-bright", "natural-dark"] {
            let bright = name == "natural-bright";
            let pixels = (0..base.len()).map(|index| {
                let w = width as usize;
                let source = if bright { index } else { index / w * w + (index + w - 70) % w };
                let mut pixel = [1.;4];
                for channel in 0..3 {
                    let value = base[source][if bright { 2 - channel } else { channel }];
                    // Match the source TIFF quantization before the float64 transform.
                    let value = if depth == 32 { value as f64 }
                        else { (value * scale as f32).round() as f64 / scale };
                    let value = if bright { value * 1.45 + 0.03 } else { value * 0.65 };
                    let value = value.clamp(0.,1.);
                    pixel[channel] = if depth == 32 { value as f32 }
                        else { ((value * scale).round_ties_even() / scale) as f32 };
                }
                pixel
            }).collect::<Vec<_>>();
            crate::io::save(&root.join(input(name,depth)),width,height,&pixels,depth)
                .with_context(|| format!("writing {name} at {depth} bits"))?;
        }
    }
    let manifest = manifest();
    let path = root.join("manifest.json");
    let file = fs::File::create(&path).with_context(|| format!("creating {}",path.display()))?;
    serde_json::to_writer_pretty(file,&manifest)?;
    println!("Generated {} cases in {}",manifest["cases"].as_array().unwrap().len(),root.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn manifest_covers_sweeps_and_inputs() {
        let manifest = manifest();
        assert_eq!(manifest["schemaVersion"],1);
        let cases = manifest["cases"].as_array().unwrap();
        let mut ids = HashSet::new();
        for c in cases {
            let id = c["id"].as_str().unwrap();
            assert!(ids.insert(id));
            assert_eq!(c["output"],format!("ps/{id}.tif"));
            assert!(c["input"].as_str().unwrap().starts_with("inputs/"));
            let depth = c["depth"].as_u64().unwrap() as u8;
            assert!(DEPTHS.contains(&depth));
            if let Some(top) = c.get("top") { assert!(top["input"].as_str().unwrap().starts_with("inputs/")); }
        }
        for depth in DEPTHS {
            let subset: Vec<_> = cases.iter().filter(|c| c["depth"] == depth).collect();
            for radius in RADII {
                assert!(subset.iter().any(|c| c["op"] == "gaussianBlur" && c["args"]["radius"]["_value"] == radius));
            }
            for angle in ANGLES { for distance in DISTANCES {
                assert!(subset.iter().any(|c| c["op"] == "motionBlur" && c["args"]["angle"] == angle && c["args"]["distance"]["_value"] == distance as f64));
            }}
            for amount in [1,10,50,100] { for quality in ["draft","good","best"] { for space in ["normalized","pixels"] {
                assert!(subset.iter().any(|c| c["op"] == "radialBlur" && c["args"]["amount"] == amount && c["args"]["blurQuality"]["_value"] == quality && c["args"]["centerSpace"] == space));
            }}}
            for mode in ["screen","linearDodge","colorDodge","multiply","color"] { for fill in [100,60,20] { for opacity in OPACITIES {
                assert!(subset.iter().any(|c| c["op"] == "layerComposite" && c["top"]["blend"] == mode && c["top"]["fill"] == fill && c["top"]["opacity"] == opacity));
            }}}
            for source in DEPTHS { if source != depth {
                assert!(subset.iter().any(|c| c["op"] == "convertMode" && c["args"]["depth"] == depth && c["input"] == input("ramp256",source)));
            }}
            for exposure in [-3.,0.,3.] { for gamma in [0.45,1.,2.2] {
                assert!(subset.iter().any(|c| c["op"] == "exposure" && c["args"]["exposure"] == exposure && c["args"]["gammaCorrection"] == gamma));
            }}
            for black in [0,16] { for white in [235,255] { for gamma in [0.45,1.,2.2] {
                assert!(subset.iter().any(|c| c["op"] == "levels" && c["args"]["adjustment"][0]["input"] == json!([black,white]) && c["args"]["adjustment"][0]["gamma"] == gamma));
            }}}
            for colorize in [false,true] {
                assert!(subset.iter().any(|c| c["op"] == "hueSaturation" && c["args"]["colorize"] == colorize));
            }
            for mode in ["normal","lighten","multiply","divide","overlay","softLight"] { for opacity in OPACITIES { for invert in [false,true] {
                assert!(subset.iter().any(|c| c["op"] == "applyImageEvent" && c["args"]["calculation"] == mode && c["args"]["opacity"] == opacity && c["args"]["invert"] == invert));
            }}}
            for blend in ["colorDodge","screen","color"] { for fill in [20,60] {
                assert!(subset.iter().any(|c| c["op"] == "copyToLayer" && c["args"] == json!({}) && c["input"] == input("rgbcube",depth) && c["top"]["input"] == input("hsv",depth) && c["top"]["blend"] == blend && c["top"]["opacity"] == 60 && c["top"]["fill"] == fill));
            }}
            assert!(subset.iter().any(|c| c["op"] == "offset" && c["args"]["horizontal"] == -257 && c["args"]["vertical"] == 513 && c["args"]["fill"]["_value"] == "repeat"));
            assert!(subset.iter().any(|c| c["op"] == "addNoise" && c["args"]["noise"]["_value"] == 0.3 && c["args"]["distort"]["_value"] == "gaussianDistribution" && c["args"]["monochromatic"] == false));
            assert!(subset.iter().any(|c| c["op"] == "Adobe Camera Raw Filter" && c["args"]["$Hi12"] == -100));
            for pattern in ["natural-bright", "natural-dark", "ramp256"] {
                let id = format!("ACR-{depth}-{pattern}-stylize");
                let case = subset.iter().find(|c| c["id"] == id).unwrap();
                assert_eq!(case["op"],"Adobe Camera Raw Filter");
                assert_eq!(case["input"],input(pattern,depth));
                assert_eq!(case["args"],json!({"$PrVN":6,"$PrVe":251920384,"$Hi12":-100}));
            }
            for op in ["levels","exposure","hueSaturation","vibrance","desaturate","blackAndWhite","gradientMapEvent","addNoise","ripple","smartSharpen","offset","applyImageEvent","Adobe Camera Raw Filter"] {
                assert!(subset.iter().any(|c| c["op"] == op),"missing {op} depth {depth}");
            }
        }
        let (w,h,pixels) = pattern("ramp4096");
        assert_eq!((w,h,pixels.len()),(4096,16,65536));
        assert_eq!(pixels[0],[0.,0.,0.,1.]);
        assert_eq!(pixels[4095],[1.,1.,1.,1.]);
        let (_,_,hdr) = pattern("hdr");
        assert_eq!(hdr.last().unwrap(),&[16.,16.,16.,1.]);
    }

    #[test]
    fn smart_sharpen_calibration_probes_are_reproducible() {
        let manifest = manifest();
        let cases = manifest["cases"].as_array().unwrap();
        for depth in DEPTHS {
            for pattern in ["natural", "sharpen-midtone"] {
                let probes: Vec<_> = cases.iter().filter(|c| {
                    c["op"] == "smartSharpen" && c["depth"] == depth && c["input"] == input(pattern,depth)
                }).collect();
                assert_eq!(probes.len(),4);
                for probe in probes {
                    let id = probe["id"].as_str().unwrap();
                    let base_id = id.replace(&format!("-{pattern}-"),"-impulse-");
                    let base = cases.iter().find(|c| c["id"] == base_id).unwrap();
                    assert_eq!(probe["args"],base["args"]);
                }
            }
        }
        let (w,h,pixels) = pattern("sharpen-midtone");
        assert_eq!((w,h,pixels.len()),(256,256,65536));
        assert_eq!(pixels[0],[0.35,0.35,0.35,1.]);
        assert!(pixels.iter().all(|p| (0.3..0.7).contains(&p[0]) && p[0] == p[1] && p[1] == p[2]));
    }
}
