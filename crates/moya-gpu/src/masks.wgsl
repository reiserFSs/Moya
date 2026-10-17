@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var output_mask: texture_storage_2d<rgba32float, write>;
@group(0) @binding(2) var<uniform> params: vec4<u32>;

fn bounded(value: f32) -> f32 {
    if value < 0.0 { return 0.0; }
    if value > 1.0 { return 1.0; }
    return value;
}

// WGSL floating division permits several ULPs of error. Divide significands
// instead, rounding ties to even, so raw ramp values match Rust f32 division.
// The caller only needs the quotient clamped to [0, 1].
fn ramp_divide(numerator: f32, denominator: f32) -> f32 {
    if numerator == 0.0 { return numerator; }
    if numerator < 0.0 { return 0.0; }
    let nb = bitcast<u32>(numerator);
    let db = bitcast<u32>(denominator);
    if db == 0x7f800000u {
        if nb == 0x7f800000u { return bitcast<f32>(0x7fc00000u); }
        return 0.0;
    }
    if numerator >= denominator { return 1.0; }
    var n = nb & 0x7fffffu;
    var d = db & 0x7fffffu;
    var ne = i32((nb >> 23u) & 255u) - 127;
    var de = i32((db >> 23u) & 255u) - 127;
    if ne == -127 {
        let shift = countLeadingZeros(n) - 8u;
        n <<= shift;
        ne = -126 - i32(shift);
    } else { n |= 0x800000u; }
    if de == -127 {
        let shift = countLeadingZeros(d) - 8u;
        d <<= shift;
        de = -126 - i32(shift);
    } else { d |= 0x800000u; }
    var exponent = ne - de;
    if n < d { n <<= 1u; exponent -= 1; }
    if exponent < -150 { return 0.0; }
    if exponent == -150 {
        return bitcast<f32>(select(0u, 1u, n > d));
    }
    let digits = u32(min(23, exponent + 149));
    var remainder = n - d;
    var quotient = 1u;
    for (var digit = 0u; digit < digits; digit += 1u) {
        remainder <<= 1u;
        quotient <<= 1u;
        if remainder >= d { remainder -= d; quotient |= 1u; }
    }
    let twice = remainder << 1u;
    if twice > d || (twice == d && (quotient & 1u) != 0u) { quotient += 1u; }
    if exponent < -126 { return bitcast<f32>(quotient); }
    // Addition also handles rounding a significand up into the next exponent.
    return bitcast<f32>((u32(exponent + 126) << 23u) + quotient);
}

fn luminosity(value: f32) -> f32 {
    let a = bitcast<f32>(params.y);
    let b = bitcast<f32>(params.z);
    let low = min(a, b);
    let high = max(a, b);
    if high <= low { return select(0.0, 1.0, value >= high); }
    return ramp_divide(value - low, high - low);
}

fn alpha_at(p: vec2<i32>, size: vec2<i32>) -> f32 {
    if any(p < vec2<i32>(0)) || any(p >= size) { return 0.0; }
    return textureLoad(source, p, 0).a;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(source);
    if any(id.xy >= size) { return; }
    let xy = vec2<i32>(id.xy);
    let pixel = textureLoad(source, xy, 0);
    var value = pixel.a;
    var zero_sign = bitcast<u32>(pixel.a) & 0x80000000u;
    if params.x == 0u {
        let ramp = luminosity(max(max(pixel.r, pixel.g), pixel.b));
        zero_sign = (bitcast<u32>(ramp) ^ bitcast<u32>(pixel.a)) & 0x80000000u;
        value = ramp * pixel.a;
    } else if params.x == 2u {
        var low = pixel.a;
        var high = pixel.a;
        let offsets = array<vec2<i32>, 4>(
            vec2<i32>(0, -1), vec2<i32>(-1, 0),
            vec2<i32>(1, 0), vec2<i32>(0, 1));
        for (var i = 0u; i < 4u; i += 1u) {
            let alpha = alpha_at(xy + offsets[i], vec2<i32>(size));
            low = min(low, alpha);
            high = max(high, alpha);
        }
        value = high - low;
        zero_sign = 0u;
    }
    if value < 0.0 { zero_sign = 0u; }
    value = bounded(value);
    // Metal may fold 0 * negative to +0; CPU masks retain IEEE signed zero.
    let exact = bitcast<f32>(select(bitcast<u32>(value), zero_sign, value == 0.0));
    textureStore(output_mask, xy, vec4<f32>(exact, exact, exact, 1.0));
}
