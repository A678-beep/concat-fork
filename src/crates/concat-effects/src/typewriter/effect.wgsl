struct Params { progress: f32 }

// Each pixel of the title is kept once the typing has reached the
// character painted there: the reveal map holds, for every character, the
// share of the typing at which it lands - 0 for the cursor, which waits
// at the end from the first frame. Everything outside the words is 0, and
// clear, so the comparison costs it nothing.
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample(uv);
    let typed = clamp(params.progress / 100.0, 0.0, 1.0);
    let shown = reveal_order(uv) <= typed;
    return select(vec4<f32>(0.0, 0.0, 0.0, 0.0), c, shown);
}
