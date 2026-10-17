@group(0) @binding(0) var src:texture_2d<f32>;
@group(0) @binding(1) var dst:texture_storage_2d<rgba8unorm,write>;
@compute @workgroup_size(16,16)
fn main(@builtin(global_invocation_id) id:vec3<u32>){if(any(id.xy>=textureDimensions(dst))){return;}textureStore(dst,vec2<i32>(id.xy),clamp(textureLoad(src,vec2<i32>(id.xy),0),vec4(0.),vec4(1.)));}
