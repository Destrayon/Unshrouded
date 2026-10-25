// The meter image drawn over the game's frame: one quad filling the viewport (the meter's rectangle), each pixel read
// 1:1 from the meter texture, blended by its alpha (the planner bakes the meter's opacity into it).

@group(0) @binding(0) var meter: texture_2d<f32>;

struct Out {
    @builtin(position) pos: vec4<f32>,
    @location(0) px: vec2<f32>,
}

@vertex
fn vs(@builtin(vertex_index) i: u32) -> Out {
    let c = vec2<f32>(f32(i & 1u), f32(i >> 1u));
    var o: Out;
    o.pos = vec4<f32>(c * 2.0 - 1.0, 0.0, 1.0);
    o.px = c * vec2<f32>(textureDimensions(meter));
    return o;
}

@fragment
fn fs(v: Out) -> @location(0) vec4<f32> {
    let size = vec2<i32>(textureDimensions(meter));
    return textureLoad(meter, clamp(vec2<i32>(v.px), vec2<i32>(0), size - 1), 0);
}
