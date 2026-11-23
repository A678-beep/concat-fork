struct Params {
    exposure: f32,
    brightness: f32,
    contrast: f32,
    saturation: f32,
    temperature: f32,
    tint: f32,
    shadows: f32,
    highlights: f32,
    whites: f32,
    blacks: f32,
    sharpen: f32,
    vignette: f32,
    fade: f32,
    lift: vec3<f32>,
    gamma: vec3<f32>,
    gain: vec3<f32>,
    luma: array<vec4<f32>, 8>,
    red: array<vec4<f32>, 8>,
    green: array<vec4<f32>, 8>,
    blue: array<vec4<f32>, 8>,
    hsl_red_hue: f32,
    hsl_red_sat: f32,
    hsl_red_lum: f32,
    hsl_orange_hue: f32,
    hsl_orange_sat: f32,
    hsl_orange_lum: f32,
    hsl_yellow_hue: f32,
    hsl_yellow_sat: f32,
    hsl_yellow_lum: f32,
    hsl_green_hue: f32,
    hsl_green_sat: f32,
    hsl_green_lum: f32,
    hsl_cyan_hue: f32,
    hsl_cyan_sat: f32,
    hsl_cyan_lum: f32,
    hsl_blue_hue: f32,
    hsl_blue_sat: f32,
    hsl_blue_lum: f32,
    hsl_purple_hue: f32,
    hsl_purple_sat: f32,
    hsl_purple_lum: f32,
    hsl_magenta_hue: f32,
    hsl_magenta_sat: f32,
    hsl_magenta_lum: f32,
    beauty_soften: f32,
    beauty_whiten: f32,
}

// The manual colour panel, in the order a colourist works: exposure and
// balance, then tone, then the edge and the frame. What a lens or a lamp
// does is done in light - exposure in stops, the white balance, the
// vignette - and contrast in stops about middle grey; what an eye judges
// on the picture - brightness, saturation, the four tonal bands, the fade,
// the wheels and curves, sharpness - in the display encoding its sliders
// were drawn in, which here is never clipped: a highlight past white is
// carried on past it. Every knob at its default is the picture as it came.

/// `by` added to a display-encoded colour, as far down as black and no
/// further: a channel that is lit is not taken below nothing.
fn lifted(encoded: vec3<f32>, by: f32) -> vec3<f32> {
    let moved = encoded + vec3<f32>(by);
    return select(moved, max(moved, vec3<f32>(0.0)), encoded >= vec3<f32>(0.0));
}

// 本包走的是「light」契约的公共库（LINEAR_LIBRARY），那里没有 display 契约
// 才有的 hsl_band / soften。0.3.2 的 HSL 与美颜用到的两段自己带一份，只用
// 公共库里确定有的东西：sample、texel、mix、smoothstep、saturation、exposure。

/// 0..1 的色相 → 一维 RGB 通道（标准 HSL 转 RGB 的中段）。
fn hue2rgb_local(p: f32, q: f32, t: f32) -> f32 {
    var tt = t;
    if (tt < 0.0) { tt += 1.0; }
    if (tt > 1.0) { tt -= 1.0; }
    if (tt < 1.0 / 6.0) { return p + (q - p) * 6.0 * tt; }
    if (tt < 0.5) { return q; }
    if (tt < 2.0 / 3.0) { return p + (q - p) * (2.0 / 3.0 - tt) * 6.0; }
    return p;
}

fn rgb_to_hsl_local(rgb: vec3<f32>) -> vec3<f32> {
    let mx = max(max(rgb.r, rgb.g), rgb.b);
    let mn = min(min(rgb.r, rgb.g), rgb.b);
    let d = mx - mn;
    var h = 0.0;
    if (d > 1e-5) {
        if (mx == rgb.r) {
            h = fract((rgb.g - rgb.b) / d / 6.0);
        } else if (mx == rgb.g) {
            h = ((rgb.b - rgb.r) / d + 2.0) / 6.0;
        } else {
            h = ((rgb.r - rgb.g) / d + 4.0) / 6.0;
        }
    }
    let l = (mx + mn) * 0.5;
    let denom = max(1.0 - abs(2.0 * l - 1.0), 1e-5);
    let s = select(d / denom, 0.0, d < 1e-5);
    return vec3<f32>(h, clamp(s, 0.0, 1.0), clamp(l, 0.0, 8.0));
}

fn hsl_to_rgb_local(hsl: vec3<f32>) -> vec3<f32> {
    if (hsl.y <= 1e-5) {
        return vec3<f32>(hsl.z);
    }
    let q = select(hsl.z + hsl.y - hsl.z * hsl.y, hsl.z * (1.0 + hsl.y), hsl.z < 0.5);
    let p = 2.0 * hsl.z - q;
    return vec3<f32>(
        hue2rgb_local(p, q, hsl.x + 1.0 / 3.0),
        hue2rgb_local(p, q, hsl.x),
        hue2rgb_local(p, q, hsl.x - 1.0 / 3.0));
}

/// 一个色相带内的 HSL 调整：只动落在 centre±width 度里的颜色。
fn hsl_band_local(rgb: vec3<f32>, centre: f32, width: f32,
    turn: f32, sat_scale: f32, lum_scale: f32) -> vec3<f32> {
    let clip = max(rgb, vec3<f32>(0.0));
    let hsl = rgb_to_hsl_local(clip);
    let degrees = hsl.x * 360.0;
    let apart = abs(degrees - centre);
    let distance = min(apart, 360.0 - apart);
    let weight = 1.0 - smoothstep(width * 0.5, width, distance);
    var moved = hsl;
    moved.x = fract(hsl.x + turn / 360.0);
    moved.y = clamp(hsl.y * sat_scale, 0.0, 1.0);
    moved.z = clamp(hsl.z * lum_scale, 0.0, 8.0);
    return mix(rgb, hsl_to_rgb_local(moved), weight);
}

/// 5×5 的方块柔化：美颜的磨皮用它，和 display 契约的 soften 同一个意思。
fn soften_local(uv: vec2<f32>, radius: f32) -> vec3<f32> {
    let t = texel() * radius * 0.5;
    var sum = vec3<f32>(0.0);
    for (var y: i32 = -2; y <= 2; y++) {
        for (var x: i32 = -2; x <= 2; x++) {
            sum += sample(uv + vec2<f32>(f32(x), f32(y)) * t).rgb;
        }
    }
    return sum / 25.0;
}
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let t = texel();
    let src = sample(uv);

    // Exposure in stops, then brightness as a lift of the display level.
    var c = exposure(src.rgb, params.exposure);
    c = from_display(lifted(to_display(c), params.brightness / 100.0 * 0.5));

    // White balance about daylight, and green to magenta.
    c = white_balance(c, params.temperature, params.tint);

    // Contrast in stops about middle grey.
    c = contrast(c, 1.0 + params.contrast / 100.0);

    // Saturation about luminance, by the display level: a push richer by
    // up to three times, a pull greyer to none at all.
    var d = to_display(c);
    let s = params.saturation / 100.0;
    d = saturation(d, select((1.0 + s) * (1.0 + s), 1.0 + 2.0 * s, s > 0.0));

    // Shadows and highlights lift or crush their half of the range, blacks
    // and whites the extreme quarter of each, by the display level; fade
    // lifts the black and carries a level past white on past it.
    let l = luma(clamp(d, vec3<f32>(0.0), vec3<f32>(1.0)));
    let low = (1.0 - smoothstep(0.0, 0.5, l)) * params.shadows / 100.0 * 0.18;
    let high = smoothstep(0.5, 1.0, l) * params.highlights / 100.0 * 0.18;
    let black = (1.0 - smoothstep(0.0, 0.25, l)) * params.blacks / 100.0 * 0.1;
    let white = smoothstep(0.75, 1.0, l) * params.whites / 100.0 * 0.1;
    d = lifted(d, low + high + black + white);
    let matte = params.fade / 100.0 * 0.25;
    let below = min(d, vec3<f32>(1.0));
    d = below * (1.0 - matte) + vec3<f32>(matte) + (d - below);

    // HSL（0.3.2）：八个色相带各转色相、调饱和、调明度，只动落在带里的
    // 像素；三样都归零时与原样一模一样。色相 ±100 转 ±50 度，饱和 -100..100
    // 是 ×0..2，明度 -100..100 是 ×0.5..1.5。
    d = hsl_band_local(d, 0.0, 45.0, params.hsl_red_hue * 0.5,
        1.0 + params.hsl_red_sat / 100.0, 1.0 + params.hsl_red_lum / 200.0);
    d = hsl_band_local(d, 30.0, 45.0, params.hsl_orange_hue * 0.5,
        1.0 + params.hsl_orange_sat / 100.0, 1.0 + params.hsl_orange_lum / 200.0);
    d = hsl_band_local(d, 60.0, 45.0, params.hsl_yellow_hue * 0.5,
        1.0 + params.hsl_yellow_sat / 100.0, 1.0 + params.hsl_yellow_lum / 200.0);
    d = hsl_band_local(d, 120.0, 50.0, params.hsl_green_hue * 0.5,
        1.0 + params.hsl_green_sat / 100.0, 1.0 + params.hsl_green_lum / 200.0);
    d = hsl_band_local(d, 180.0, 45.0, params.hsl_cyan_hue * 0.5,
        1.0 + params.hsl_cyan_sat / 100.0, 1.0 + params.hsl_cyan_lum / 200.0);
    d = hsl_band_local(d, 240.0, 50.0, params.hsl_blue_hue * 0.5,
        1.0 + params.hsl_blue_sat / 100.0, 1.0 + params.hsl_blue_lum / 200.0);
    d = hsl_band_local(d, 285.0, 45.0, params.hsl_purple_hue * 0.5,
        1.0 + params.hsl_purple_sat / 100.0, 1.0 + params.hsl_purple_lum / 200.0);
    d = hsl_band_local(d, 320.0, 40.0, params.hsl_magenta_hue * 0.5,
        1.0 + params.hsl_magenta_sat / 100.0, 1.0 + params.hsl_magenta_lum / 200.0);

    // The wheels, then the curves, on the display level too.
    d = grade_wheels(d, params.lift, params.gamma, params.gain);
    d = grade_curves(d, params.luma, params.red, params.green, params.blue);

    // Sharpen: the picture's difference from a small blur of it, scaled.
    if (params.sharpen > 0.0) {
        var blur = vec3<f32>(0.0);
        for (var y: i32 = -1; y <= 1; y++) {
            for (var x: i32 = -1; x <= 1; x++) {
                blur += to_display(sample(uv + vec2<f32>(f32(x), f32(y)) * t).rgb);
            }
        }
        blur /= 9.0;
        d = d + (to_display(src.rgb) - blur) * params.sharpen / 100.0 * 2.0;
    }
    c = from_display(d);

    // Vignette: the light falling off towards the corners, as a lens's does.
    if (params.vignette > 0.0) {
        let r = length((uv - vec2<f32>(0.5)) * vec2<f32>(2.0));
        let v = params.vignette / 100.0;
        let fall = smoothstep(1.4 - v * 0.9, 1.6 - v * 0.3, r);
        c = c * pow(1.0 - fall * (0.4 + 0.6 * v), 2.4);
    }

    // 美颜（0.3.2）：磨皮是整画面柔化后混回来，美白在光里提亮并压一点艳度。
    // 瘦脸/大眼/身材要人脸关键点模型，本版不做，界面如实标即将开放。
    if (params.beauty_soften > 0.0) {
        c = mix(c, soften_local(uv, 3.5), params.beauty_soften / 100.0 * 0.75);
    }
    if (params.beauty_whiten > 0.0) {
        let w = params.beauty_whiten / 100.0;
        c = exposure(c, w * 0.45);
        c = saturation(c, 1.0 - w * 0.1);
    }

    return vec4<f32>(c, src.a);
}
