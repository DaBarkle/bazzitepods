// The purist island's body, key line and border light, drawn analytically per pixel:
// a signed distance to the rounded rectangle gives exact anti-aliased edges at any
// scale, and every glow is a smooth function of that distance, so there are no
// polygon seams, banding steps or blur passes. Everything is in physical pixels.

struct Uniforms {
    // Center (x, y) relative to the widget, and half size (w, h).
    rect: vec4<f32>,
    // Corner radius, physical px per logical px, body opacity, 1 when the target is sRGB.
    params: vec4<f32>,
    // Key line colour (straight, sRGB) and opacity.
    key: vec4<f32>,
    // Glow colour (straight, sRGB) and strength.
    glow_color: vec4<f32>,
    // Glow kind (0 none, 1 sweep, 2 breathe, 3 flow, 4 ping), progress 0..1, age in
    // seconds, and +1 / -1 for an island anchored to the top / bottom of the screen.
    glow: vec4<f32>,
    // Widget origin in the framebuffer, physical px.
    origin: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u: Uniforms;

const PI: f32 = 3.14159265;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // One triangle covering the widget's viewport.
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

fn sd_round_box(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - b + vec2<f32>(r);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

// Distance travelled along the outline from the middle of the anchored edge, as 0..1
// of the way to the middle of the opposite edge (the same on both sides, so lights
// started at the top run down both sides and meet at the bottom).
fn along(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let ax = abs(p.x);
    let y = p.y;
    let sx = max(b.x - r, 0.0);
    let sy = max(b.y - r, 0.0);
    let quarter = 0.5 * PI * r;
    let half = 2.0 * sx + 2.0 * quarter + 2.0 * sy;
    var s: f32;
    if (ax <= sx) {
        s = select(half - ax, ax, y < 0.0);
    } else if (abs(y) <= sy) {
        s = sx + quarter + (y + sy);
    } else if (y < 0.0) {
        s = sx + r * atan2(ax - sx, -(y + sy));
    } else {
        s = sx + quarter + 2.0 * sy + r * atan2(y - sy, ax - sx);
    }
    return s / max(half, 0.001);
}

// Apple Intelligence-like colours, flowing: violet, pink, orange, coral, blue.
fn spectrum(x: f32) -> vec3<f32> {
    let c0 = vec3<f32>(0.737, 0.510, 0.953);
    let c1 = vec3<f32>(0.961, 0.725, 0.918);
    let c2 = vec3<f32>(1.000, 0.729, 0.443);
    let c3 = vec3<f32>(1.000, 0.404, 0.471);
    let c4 = vec3<f32>(0.553, 0.624, 1.000);
    let t = fract(x) * 5.0;
    let i = floor(t);
    let f = smoothstep(0.0, 1.0, t - i);
    if (i < 1.0) { return mix(c0, c1, f); }
    if (i < 2.0) { return mix(c1, c2, f); }
    if (i < 3.0) { return mix(c2, c3, f); }
    if (i < 4.0) { return mix(c3, c4, f); }
    return mix(c4, c0, f);
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

// Tiny per-pixel noise so soft glows don't band on 8-bit surfaces.
fn dither(p: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(p, vec2<f32>(0.06711056, 0.00583715)))) - 0.5;
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let unit = u.params.y;
    let p = frag.xy - u.origin.xy - u.rect.xy;
    let b = u.rect.zw;
    let r = min(u.params.x, min(b.x, b.y));
    let d = sd_round_box(p, b, r);
    let dl = d / unit;

    // Solid black body with a one pixel anti-aliased edge.
    let body = clamp(0.5 - d, 0.0, 1.0) * u.params.z;

    // Key line: a hairline (one logical px) just inside the edge.
    let kw = max(unit, 1.0);
    let key_cov = clamp(0.5 + min(d + kw, -d), 0.0, 1.0);
    let ka = key_cov * u.key.a * u.params.z;
    var rgb = u.key.rgb * ka;
    var a = ka + body * (1.0 - ka);

    let kind = u.glow.x;
    if (kind > 0.5 && u.glow_color.a > 0.0) {
        let t = u.glow.y;
        let age = u.glow.z;
        let s = along(vec2<f32>(p.x, p.y * u.glow.w), b, r);
        var color = u.glow_color.rgb;
        var intensity = 0.0;
        var ripple = 0.0;
        if (kind < 1.5) {
            // Sweep: a bright head with a long tail, easing out as the two heads meet,
            // then the whole line glows softly and fades.
            let head = 1.3 * (1.0 - pow(1.0 - t, 2.4));
            let behind = head - s;
            let trail = select(exp(-behind * 4.5), exp(behind * 30.0), behind < 0.0);
            let env = smoothstep(0.0, 0.05, t) * (1.0 - smoothstep(0.5, 1.0, t));
            let settle = 0.3 * smoothstep(0.2, 0.55, t) * (1.0 - smoothstep(0.55, 1.0, t));
            intensity = env * trail + settle;
        } else if (kind < 2.5) {
            // Breathe: in and out twice.
            let w = sin(2.0 * PI * t);
            intensity = (0.12 + 0.88 * w * w) * smoothstep(0.0, 0.04, t)
                * (1.0 - smoothstep(0.85, 1.0, t));
        } else if (kind < 3.5) {
            // Flow: colours drift round the border with a slow swell.
            color = spectrum(s * 0.85 - age * 0.32);
            let swell = 0.72 + 0.28 * sin(2.0 * PI * (s * 1.5) - age * 6.0);
            intensity = swell * smoothstep(0.0, 0.16, t) * (1.0 - smoothstep(0.5, 1.0, t));
        } else {
            // Ping: a flash, and a ripple that leaves the edge.
            if (u.glow_color.a > 1.5) {
                color = spectrum(s * 0.85 - age * 0.32);
            }
            intensity = exp(-t * 4.0) * smoothstep(0.0, 0.03, t);
            let at = 8.0 * (1.0 - pow(1.0 - t, 2.0));
            let off = dl - at;
            ripple = select(0.0, 0.4 * pow(1.0 - t, 2.0) * exp(-off * off / 6.0), d > 0.0);
        }
        intensity = intensity * min(u.glow_color.a, 1.0);

        // Hot core on the key line, a soft bloom outside, and light falling inwards
        // over the black (the way Siri's glow washes in from the edge).
        let core = intensity * key_cov;
        let bloom = select(0.0, 0.6 * intensity * exp(-dl * dl / 18.0), d > 0.0);
        let inner = select(0.0, 0.32 * intensity * exp(-dl * dl / 70.0), d <= 0.0);
        let la = clamp(core + bloom + inner + ripple, 0.0, 1.0) * u.params.z;
        // The brightest part of the core runs a little towards white, like a hot filament.
        let lit = mix(color, vec3<f32>(1.0), 0.35 * core);
        rgb = lit * la + rgb * (1.0 - la);
        a = la + a * (1.0 - la);
        let n = dither(frag.xy) / 255.0;
        rgb = max(rgb + vec3<f32>(n) * la, vec3<f32>(0.0));
    }

    if (a <= 0.0) {
        discard;
    }
    if (u.params.w > 0.5) {
        // Premultiplied sRGB to premultiplied linear.
        rgb = to_linear(rgb / a) * a;
    }
    return vec4<f32>(rgb, a);
}
