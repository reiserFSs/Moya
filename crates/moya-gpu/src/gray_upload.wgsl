@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var destination: texture_storage_2d<rgba32float, write>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (any(id.xy >= textureDimensions(destination))) { return; }
    let gray = textureLoad(source, vec2<i32>(id.xy), 0).r;
    textureStore(destination, vec2<i32>(id.xy), vec4<f32>(gray, gray, gray, 1.0));
}
