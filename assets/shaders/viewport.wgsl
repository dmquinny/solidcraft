
struct U {
    vp: mat4x4<f32>,
    back: vec4<f32>,   // xyz = toward the eye, w = 1 for linear output
    screen: vec4<f32>, // viewport width, height (px), unused, silhouettes on (1)
    clip: vec4<f32>,   // section plane: normal, d (zero normal: no section)
    cap: vec4<f32>,    // colour of the inside of cut bodies
    ana: vec4<f32>,    // surface analysis: mode (1 zebra, 2 draft, 3 curvature), parameter
    pull: vec4<f32>,   // draft pull direction
    acc_u: vec4<f32>,  // accessibility map axes (xyz, offset)
    acc_v: vec4<f32>,
    eye: vec4<f32>,    // eye position, w = 1 for perspective (0: orthographic, looking along -back)
    ao: vec4<f32>,     // radius in world units (zero: disabled)
    // analysis colours (`theme::AnalysisColors`): draft positive, between, negative; reachable,
    // blocked; curvature flat, middle, tight
    ana_c: array<vec4<f32>, 8>,
    ana_f: vec4<f32>,  // x = 1: stripe the negative draft and blocked faces
    depth: vec4<f32>,  // x = 1: reversed depth (1 near, 0 far); y, z: the faces' push (`FACE_BIAS`)
};

fn reversed() -> bool {
    return u.depth.x > 0.5;
}

/// Clip-space depth measured the usual way (0 at the near plane, w at the far one), whichever
/// way the depth buffer runs.
fn near_z(c: vec4<f32>) -> f32 {
    return select(c.z, c.w - c.z, reversed());
}

@group(1) @binding(2) var acc_t: texture_2d<f32>;
@group(2) @binding(0) var ao_depth: texture_2d<u32>;

fn clipped(p: vec3<f32>) -> bool {
    return length(u.clip.xyz) > 0.5 && dot(u.clip.xyz, p) > u.clip.w;
}
@group(0) @binding(0) var<uniform> u: U;

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    return select(c, pow((c + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4)), c > vec3<f32>(0.04045));
}

fn out_color(c: vec4<f32>) -> vec4<f32> {
    if (u.back.w > 0.5) {
        return vec4<f32>(to_linear(c.rgb), c.a);
    }
    return c;
}

struct TOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) n: vec3<f32>,
    @location(1) c: vec4<f32>,
    @location(2) wp: vec3<f32>,
    // A planned body's surface pattern (`pattern_color`; zeros: none).
    @location(3) @interpolate(flat) pat: vec4<u32>,
    @location(4) @interpolate(flat) pc: vec4<f32>,
    @location(5) @interpolate(flat) org: vec3<f32>,
};

@vertex
fn vs_tri(@location(0) p: vec3<f32>, @location(1) n: vec3<f32>, @location(2) c: vec4<f32>) -> TOut {
    var o: TOut;
    o.pos = u.vp * vec4<f32>(p, 1.0);
    o.n = n;
    o.c = c;
    o.wp = p;
    o.pat = vec4<u32>(0u);
    o.pc = vec4<f32>(0.0);
    o.org = vec3<f32>(0.0);
    return o;
}

/// Planned bodies: `vs_tri` plus each vertex's surface pattern, second colour and anchor.
@vertex
fn vs_planned(
    @location(0) p: vec3<f32>,
    @location(1) n: vec3<f32>,
    @location(2) c: vec4<f32>,
    @location(3) pat: vec4<u32>,
    @location(4) pc: vec4<f32>,
    @location(5) org: vec3<f32>,
) -> TOut {
    var o: TOut;
    o.pos = u.vp * vec4<f32>(p, 1.0);
    o.n = n;
    o.c = c;
    o.wp = p;
    o.pat = pat;
    o.pc = pc;
    o.org = org;
    return o;
}

/// Opaque model surfaces: cut by the section plane, the inside shows as a flat cap colour.
@fragment
fn fs_solid(i: TOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let c = solid(i, front);
    return c;
}

struct CutOut {
    @location(0) c: vec4<f32>,
    @builtin(frag_depth) depth: f32,
};

/// The section cap (the inside seen through the cut, drawn on the far walls' back faces where
/// they are the visible surface) again, at the depth of the section plane rather than of the
/// far wall, so the cut face hides what lies inside the body instead of showing it like a
/// pane of glass.
@fragment
fn fs_solid_cut(i: TOut, @builtin(front_facing) front: bool) -> CutOut {
    var o: CutOut;
    // Derivatives (in `pushed`) first, before `solid` may discard.
    o.depth = pushed(cap_depth(i.wp, i.pos.z));
    o.c = solid(i, front);
    return o;
}

/// A depth the shader writes itself, pushed back like the faces the GPU pushes (`FACE_BIAS`,
/// which does not apply then): its own slope over `depth.z` pixels and `depth.y` depth steps.
fn pushed(z: f32) -> f32 {
    let slope = max(abs(dpdx(z)), abs(dpdy(z)));
    // One step: 2⁻²⁴ in a 24-bit buffer; a float buffer's steps follow the value (2⁻²³ of it).
    let step = select(1.0 / 16777216.0, abs(z) * 1.1920929e-7, reversed());
    let push = slope * u.depth.z + step * u.depth.y;
    return clamp(z - select(-push, push, reversed()), 0.0, 1.0);
}

/// Depth of the section plane on the view ray through `wp` (a far wall seen through the cut),
/// never behind the wall itself (`z`).
fn cap_depth(wp: vec3<f32>, z: f32) -> f32 {
    var dir = -u.back.xyz;
    if (u.eye.w > 0.5) { dir = wp - u.eye.xyz; }
    let dn = dot(u.clip.xyz, dir);
    if (abs(dn) < 1e-9) { return z; }
    let p = wp + dir * ((u.clip.w - dot(u.clip.xyz, wp)) / dn);
    let c = u.vp * vec4<f32>(p, 1.0);
    if (c.w <= 0.0) { return z; }
    if (reversed()) { return clamp(c.z / c.w, z, 1.0); }
    return clamp(c.z / c.w, 0.0, z);
}

fn solid(i: TOut, front: bool) -> vec4<f32> {
    // Derivatives first, in uniform control flow (browsers' WGSL compilers reject them after
    // a branch that returns; the whole shader module then fails and nothing is drawn).
    let k = curvature(i);
    let ndv = dot(normalize(i.n), normalize(u.back.xyz));
    let fw = fwidth(ndv);
    // Millimetres per pixel, so a pattern's finest detail fades instead of shimmering.
    let foot = length(fwidth(i.wp));
    if (clipped(i.wp)) { discard; }
    if (!front && length(u.clip.xyz) > 0.5) {
        // The cut face in its body's colour ("from component" colouring), tinted toward the
        // cap colour so it reads as a cut, hatched at 45 degrees on screen.
        let base = mix(i.c.rgb, u.cap.rgb, 0.35);
        let h = fract((i.pos.x + i.pos.y) / 9.0);
        return out_color(vec4<f32>(select(base, base * 0.6, h < 0.22), 1.0));
    }
    if (u.ana.x > 0.5 && length(i.n) > 0.5) {
        return analysis(i, k);
    }
    let c = lit(i, occlusion(i), foot);
    // Silhouettes (styles with edges): where a curved surface turns away from the eye, a line
    // about 1.5 px wide, on the visible surface only (depth-correct).
    if (u.screen.w > 0.5 && length(i.n) > 0.5 && fw > 1e-6) {
        let a = 1.0 - smoothstep(fw * 0.8, fw * 2.0, abs(ndv));
        return vec4<f32>(mix(c.rgb, out_color(vec4<f32>(0.07, 0.08, 0.10, 1.0)).rgb, a), c.a);
    }
    return c;
}

// A single-sample opaque depth map, independent of the window's MSAA and its UI offsets.
// Preserve the depth's bits in R32Uint: GLSL cannot textureLoad a depth texture,
// and float colour attachments are not available on every GL adapter.
struct AoDepth {
    @builtin(frag_depth) depth: f32,
    @location(0) value: u32,
};

@fragment
fn fs_ao_depth(i: TOut, @builtin(front_facing) front: bool) -> AoDepth {
    if (clipped(i.wp) || length(i.n) < 0.5) { discard; }
    var depth = i.pos.z;
    if (!front && length(u.clip.xyz) > 0.5) { depth = cap_depth(i.wp, i.pos.z); }
    return AoDepth(depth, bitcast<u32>(depth));
}

// Deterministic cosine-weighted hemisphere samples. Projecting the sample and a point one
// radius toward the eye supplies a depth range in both orthographic and perspective views;
// distant foreground geometry cannot cast a dark halo onto a background surface.
fn occlusion(i: TOut) -> f32 {
    let radius = u.ao.x;
    if (radius <= 0.0 || length(i.n) < 0.5) { return 1.0; }
    var n = normalize(i.n);
    if (dot(n, u.back.xyz) < 0.0) { n = -n; }
    let axis = select(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 1.0, 0.0), abs(n.z) > 0.9);
    let tangent = normalize(cross(axis, n));
    let bitangent = cross(n, tangent);
    let size = vec2<i32>(textureDimensions(ao_depth));
    // The map is a 1 × 1 stand-in until a frame draws into it (the product preview draws no
    // prepass): sampling it tells nothing, so leave the surface unoccluded.
    if (size.x < 2 || size.y < 2) { return 1.0; }
    // One comparison for both depth directions: depths read as 0 near / 1 far (mirrored when
    // the buffer runs reversed). The map is cleared to its far plane either way, so a texel
    // with nothing in it reads as 1 and the `d < 1.0` test below skips it.
    let flip = reversed();
    var blocked = 0.0;
    for (var j = 0u; j < 32u; j = j + 1u) {
        let f = (f32(j) + 0.5) / 32.0;
        let angle = f32(j) * 2.39996323;
        let direction = tangent * (cos(angle) * sqrt(f)) + bitangent * (sin(angle) * sqrt(f)) + n * sqrt(1.0 - f);
        let reach = radius * (0.2 + 0.8 * fract(f32(j) * 0.61803399 + 0.5));
        let sample = i.wp + n * (radius * 0.025) + direction * reach;
        let q = u.vp * vec4<f32>(sample, 1.0);
        let near = u.vp * vec4<f32>(sample + u.back.xyz * radius, 1.0);
        if (q.w <= 0.0 || near.w <= 0.0) { continue; }
        let ndc = q.xyz / q.w;
        let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
        if (any(uv < vec2<f32>(0.0)) || any(uv >= vec2<f32>(1.0)) || ndc.z < 0.0 || ndc.z > 1.0) { continue; }
        let d0 = bitcast<f32>(textureLoad(ao_depth, vec2<i32>(uv * vec2<f32>(size)), 0).x);
        let zs = select(ndc.z, 1.0 - ndc.z, flip);
        let zn = select(near.z / near.w, 1.0 - near.z / near.w, flip);
        let d = select(d0, 1.0 - d0, flip);
        let span = max(zs - zn, 1e-7);
        let delta = (zs - d) / span;
        if (d < 1.0 && delta > 0.005 && delta < 1.0) {
            blocked = blocked + smoothstep(0.005, 0.15, delta) * (1.0 - smoothstep(0.5, 1.0, delta));
        }
    }
    return 1.0 - 0.85 * blocked / 32.0;
}

/// How fast the normal turns across the pixel (per mm).
fn curvature(i: TOut) -> f32 {
    let nn = normalize(i.n);
    let kx = length(dpdx(nn)) / max(length(dpdx(i.wp)), 1e-6);
    let ky = length(dpdy(nn)) / max(length(dpdy(i.wp)), 1e-6);
    return max(kx, ky);
}

/// Surface analysis colours: zebra stripes, draft angle bands, curvature map (`k`: the
/// surface's curvature here, worked out by the caller).
fn analysis(i: TOut, k: f32) -> vec4<f32> {
    let v = normalize(u.back.xyz);
    var n = normalize(i.n);
    if (dot(n, v) < 0.0) { n = -n; }
    let lit = 0.55 + 0.45 * max(dot(n, v), 0.0);
    if (u.ana.x < 1.5) {
        // Reflections of upright light bars standing around the viewer: the stripe follows
        // the reflected ray's sideways direction (horizontal on screen).
        let r = reflect(-v, n);
        var side = cross(v, vec3<f32>(0.0, 0.0, 1.0));
        if (length(side) < 1e-3) { side = vec3<f32>(1.0, 0.0, 0.0); }
        let s = sin(dot(r, normalize(side)) * u.ana.y * 3.14159265);
        let c = select(0.08, 0.95, s > 0.0);
        return out_color(vec4<f32>(vec3<f32>(c), 1.0));
    }
    if (u.ana.x > 4.5) {
        // Reachable from the access direction: facing it and nothing above.
        let d = normalize(u.pull.xyz);
        let size = i32(u.ana.w);
        let px = vec2<i32>(floor(vec2<f32>(dot(i.wp, u.acc_u.xyz) + u.acc_u.w, dot(i.wp, u.acc_v.xyz) + u.acc_v.w) * u.ana.z));
        var top = -1e30;
        if (px.x >= 0 && px.y >= 0 && px.x < size && px.y < size) {
            top = textureLoad(acc_t, px, 0).x;
        }
        let cn = dot(normalize(i.n), d);
        let facing = cn > 0.02;
        // Slack of a pixel and a half, more on slopes (the map holds the pixel's highest point).
        let slack = u.ana.y * (1.0 + sqrt(max(1.0 - cn * cn, 0.0)) / max(cn, 0.05));
        let clear = dot(i.wp, d) >= top - slack;
        var c = u.ana_c[4].rgb;
        if (facing && clear) { c = u.ana_c[3].rgb; } else { c = stripe(i, c); }
        return out_color(vec4<f32>(c * lit, 1.0));
    }
    if (u.ana.x > 3.5) {
        // A studio around the model: sky above, a dark floor below, a bright horizon band and
        // two soft light panels, seen in a mirror.
        let r = reflect(-v, n);
        var c = mix(vec3<f32>(0.32, 0.34, 0.38), vec3<f32>(0.80, 0.86, 0.95), smoothstep(-0.05, 0.6, r.z));
        c = c + vec3<f32>(0.6) * exp(-abs(r.z) * 40.0);
        let side = atan2(r.y, r.x);
        c = c + vec3<f32>(0.9) * smoothstep(0.92, 0.98, cos(side * 2.0)) * smoothstep(0.1, 0.3, r.z) * smoothstep(0.8, 0.6, r.z);
        if (r.z < -0.02) {
            // The floor: lighter toward the horizon, with light-panel streaks along it.
            c = mix(vec3<f32>(0.55, 0.56, 0.60), vec3<f32>(0.20, 0.20, 0.22), clamp(-r.z * 1.6, 0.0, 1.0));
            c = c + vec3<f32>(0.35) * smoothstep(0.95, 0.99, cos(side * 3.0));
        }
        return out_color(vec4<f32>(min(c, vec3<f32>(1.0)), 1.0));
    }
    if (u.ana.x < 2.5) {
        let a = degrees(asin(clamp(dot(normalize(i.n), normalize(u.pull.xyz)), -1.0, 1.0)));
        var c = u.ana_c[1].rgb;
        if (a >= u.ana.y) { c = u.ana_c[0].rgb; }
        if (a <= -u.ana.y) { c = stripe(i, u.ana_c[2].rgb); }
        return out_color(vec4<f32>(c * lit, 1.0));
    }
    // Curvature from how fast the normal turns across the pixel.
    let t = clamp(k * u.ana.y, 0.0, 1.0);
    let c = select(mix(u.ana_c[6].rgb, u.ana_c[7].rgb, t * 2.0 - 1.0), mix(u.ana_c[5].rgb, u.ana_c[6].rgb, t * 2.0), t < 0.5);
    return out_color(vec4<f32>(c * lit, 1.0));
}

/// A "bad" analysis face (negative draft, blocked): with the stripes on (colour vision modes), the
/// section cap's 45 degree hatch over it, so colour is not the only cue.
fn stripe(i: TOut, c: vec3<f32>) -> vec3<f32> {
    if (u.ana_f.x < 0.5) { return c; }
    let h = fract((i.pos.x + i.pos.y) / 9.0);
    return select(c, c * 0.6, h < 0.22);
}

@fragment
fn fs_tri(i: TOut) -> @location(0) vec4<f32> {
    if (clipped(i.wp)) { discard; }
    return shade(i);
}

/// Reference planes: shaded like `fs_tri`, but never cut by the section plane.
@fragment
fn fs_tri_all(i: TOut) -> @location(0) vec4<f32> {
    return shade(i);
}

// Surface patterns (a self-hosting patch, not upstream): wood grain, brushed, blasted and mottled
// finishes, worked out from the surface point itself, so they need no texture or coordinates.
// `pat`: kind (0 none, 1 wood, 2 brushed, 3 blasted, 4 mottled, 5 speckled, 6 veined, 7 knotty
// wood, 8 plywood, 9 strands), grain axis (0 x, 1 y, 2 z; two bits up: the part's thinnest side),
// scale (a quarter millimetre per step), contrast (0-255). `pc`: the second colour, its alpha
// the body's seed. `org`: the body's anchor (its box centre), so the pattern stays with it.

fn pat_hash(p: vec3<f32>) -> f32 {
    var q = fract(p * vec3<f32>(0.1031, 0.1030, 0.0973));
    q = q + dot(q, q.yxz + vec3<f32>(33.33));
    return fract((q.x + q.y) * q.z);
}

fn pat_noise(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let w = f * f * (vec3<f32>(3.0) - 2.0 * f);
    let x00 = mix(pat_hash(i), pat_hash(i + vec3<f32>(1.0, 0.0, 0.0)), w.x);
    let x10 = mix(pat_hash(i + vec3<f32>(0.0, 1.0, 0.0)), pat_hash(i + vec3<f32>(1.0, 1.0, 0.0)), w.x);
    let x01 = mix(pat_hash(i + vec3<f32>(0.0, 0.0, 1.0)), pat_hash(i + vec3<f32>(1.0, 0.0, 1.0)), w.x);
    let x11 = mix(pat_hash(i + vec3<f32>(0.0, 1.0, 1.0)), pat_hash(i + vec3<f32>(1.0, 1.0, 1.0)), w.x);
    return mix(mix(x00, x10, w.y), mix(x01, x11, w.y), w.z);
}

fn pat_fbm(p: vec3<f32>) -> f32 {
    var s = 0.0;
    var a = 0.5;
    var q = p;
    for (var k = 0; k < 4; k = k + 1) {
        s = s + a * pat_noise(q);
        q = q * 2.03 + vec3<f32>(17.1, 9.2, 3.7);
        a = a * 0.5;
    }
    return s;
}

/// The point in the body's frame: (along the grain axis, then the two across it).
fn pat_frame(q: vec3<f32>, axis: u32) -> vec3<f32> {
    if (axis == 0u) { return vec3<f32>(q.x, q.y, q.z); }
    if (axis == 1u) { return vec3<f32>(q.y, q.z, q.x); }
    return vec3<f32>(q.z, q.x, q.y);
}

/// How much of a detail `size` mm across survives at `foot` mm per pixel (1: all, 0: averaged
/// away), so fine grain fades instead of shimmering when the view zooms out.
fn pat_keep(size: f32, foot: f32) -> f32 {
    return 1.0 - smoothstep(size * 0.35, size * 0.9, foot);
}

/// Wood in the frame `f` (along the grain, then across it): growth rings around a pith outside
/// the part, wobbling along the grain, early and late wood, fibres streaking along the axis and,
/// with `knots`, knots where branches grew out of the pith, the rings flowing round them. `side`
/// (an angle across the grain, or negative for anywhere) puts the pith under a face, as in a
/// flat-sawn board, so that face's knots come out round.
fn wood_color(base: vec3<f32>, late_color: vec3<f32>, f: vec3<f32>, s: f32, contrast: f32, seed: f32, foot: f32, knots: bool, side: f32) -> vec3<f32> {
    var angle = seed * 6.2831853;
    if (side >= 0.0) {
        angle = side + (seed - 0.5) * 0.7;
    }
    let pith = vec2<f32>(cos(angle), sin(angle)) * (s * 9.0 + 25.0 + seed * 45.0);
    let d = f.yz - pith;
    let wobble = select(3.2, 2.0, knots);
    let warp = (pat_fbm(vec3<f32>(f.x * 0.012, f.y * 0.06, f.z * 0.06) + vec3<f32>(seed * 31.0)) - 0.5) * s * wobble;
    var r = length(d) + warp;
    var knot = 0.0;
    var knot_r = 0.0;
    var dead = false;
    // The nearest knot's distance in its own radii, for the light halo round it.
    var near = 9.0;
    if (knots) {
        // Branches leave the pith at random places along the trunk, about one every 120 mm,
        // heading roughly toward the part (a board shows the knots of branches that crossed it).
        let spacing = 120.0;
        let toward = atan2(-pith.y, -pith.x);
        let c0 = floor(f.x / spacing);
        for (var k = -1; k <= 1; k = k + 1) {
            let cell = c0 + f32(k);
            if (pat_hash(vec3<f32>(cell, seed * 13.0, 1.0)) > 0.85) { continue; }
            let along = (cell + 0.2 + 0.6 * pat_hash(vec3<f32>(cell, seed * 13.0, 2.0))) * spacing;
            let a = toward + (pat_hash(vec3<f32>(cell, seed * 13.0, 3.0)) - 0.5) * 0.9;
            let u = vec2<f32>(cos(a), sin(a));
            let outward = dot(d, u);
            if (outward <= 0.0) { continue; }
            let across = d.x * u.y - d.y * u.x;
            // Mostly small knots, now and then a big one.
            let radius = (3.0 + 14.0 * pow(pat_hash(vec3<f32>(cell, seed, 4.0)), 1.5)) * clamp(0.4 + outward / 150.0, 0.4, 1.0);
            let db = length(vec2<f32>(f.x - along, across));
            // The grain wraps round the knot.
            r = r + radius * 1.8 * exp(-(db * db) / (radius * radius * 3.0));
            near = min(near, db / radius);
            let inside = 1.0 - smoothstep(radius * 0.9, radius, db);
            if (inside > knot) {
                knot = inside;
                knot_r = db / radius;
                dead = pat_hash(vec3<f32>(cell, seed, 5.0)) < 0.3;
            }
        }
    }
    let t = fract(r / s);
    // Pine's latewood (the knotty kind) is a thin line; other woods' a broader band.
    var late = smoothstep(0.5, 0.85, t) * (1.0 - smoothstep(0.92, 1.0, t));
    if (knots) {
        late = smoothstep(0.74, 0.9, t) * (1.0 - smoothstep(0.95, 1.0, t));
    }
    let rings = mix(select(0.3, 0.15, knots), late, pat_keep(s, foot));
    var c = mix(base, late_color, rings * contrast);
    let fibre = pat_noise(vec3<f32>(f.x * 0.04, f.y * 3.0, f.z * 3.0) + vec3<f32>(seed * 7.0));
    c = c * (1.0 + (fibre - 0.5) * 0.18 * pat_keep(0.6, foot));
    if (knots) {
        // Just round a knot the wood is lighter, where the grain swirls past.
        c = c * (1.0 + 0.09 * smoothstep(1.0, 1.25, near) * (1.0 - smoothstep(1.25, 2.8, near)));
    }
    if (knot > 0.0) {
        // The knot: reddish amber with fine rings of its own, darker toward a dark rim; a dead
        // knot's rim is nearly black.
        let amber = late_color * vec3<f32>(0.95, 0.66, 0.46);
        let kr = fract(knot_r * 5.0 + 0.3);
        var kc = mix(amber, amber * 0.76, smoothstep(0.3, 0.75, kr) * pat_keep(1.0, foot));
        kc = mix(kc, amber * 0.66, smoothstep(0.3, 0.9, knot_r));
        let rim = select(amber * 0.45, vec3<f32>(0.1, 0.06, 0.04), dead);
        kc = mix(kc, rim, smoothstep(select(0.8, 0.66, dead), 0.97, knot_r));
        c = mix(c, kc, knot);
    }
    return clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn pattern_color(i: TOut, foot: f32) -> vec3<f32> {
    let kind = i.pat.x;
    let base = i.c.rgb;
    if (kind == 0u) { return base; }
    let s = max(f32(i.pat.z) * 0.25, 0.25);
    let contrast = f32(i.pat.w) / 255.0;
    let seed = i.pc.a;
    let axis = i.pat.y & 3u;
    let thin = (i.pat.y >> 2u) & 3u;
    let f = pat_frame(i.wp - i.org, axis);
    if (kind == 1u || kind == 7u) {
        // Knotty boards are flat-sawn: the pith lies beyond their wide face (below or above, as
        // the seed has it), across the part's thinnest side.
        var side = -1.0;
        if (kind == 7u) {
            if (thin == (axis + 1u) % 3u) {
                side = 0.0;
            } else if (thin == (axis + 2u) % 3u) {
                side = 1.5707963;
            }
            if (side >= 0.0 && fract(seed * 7.0) > 0.5) {
                side = side + 3.1415927;
            }
        }
        return wood_color(base, i.pc.rgb, f, s, contrast, seed, foot, kind == 7u, side);
    }
    if (kind == 8u) {
        // Plywood: plies across the part's thinnest side, each a veneer whose grain crosses the
        // one before, a shade of its own, and a dark glue line between; a face shows its veneer,
        // an edge the plies.
        let q = i.wp - i.org;
        let t = q[min(thin, 2u)] / s;
        let layer = floor(t);
        let odd = fract(layer * 0.5) > 0.25;
        var grain = axis;
        if (odd && thin != axis) { grain = 3u - thin - axis; }
        let veneer = wood_color(base, i.pc.rgb, pat_frame(q, min(grain, 2u)), 3.0, contrast, fract(seed + layer * 0.37), foot, false, -1.0);
        let shade = 0.92 + 0.12 * pat_hash(vec3<f32>(layer, seed * 5.0, 7.0));
        let glue = smoothstep(0.0, 0.07 + foot / s, min(fract(t), 1.0 - fract(t)));
        return clamp(veneer * shade * (0.72 + 0.28 * glue), vec3<f32>(0.0), vec3<f32>(1.0));
    }
    if (kind == 9u) {
        // Strands (OSB): flakes three times as long as wide along the axis, the nearest one's
        // shade wins, darker where two meet, and a little fibre in each.
        let w = max(s, foot * 3.0);
        let q = vec3<f32>(f.x / (w * 3.0), f.y / w, f.z / w);
        let cell = floor(q);
        var d1 = 9.0;
        var d2 = 9.0;
        var shade = 0.0;
        for (var x = -1; x <= 1; x = x + 1) {
            for (var y = -1; y <= 1; y = y + 1) {
                for (var z = -1; z <= 1; z = z + 1) {
                    let c = cell + vec3<f32>(f32(x), f32(y), f32(z));
                    let h = vec3<f32>(pat_hash(c + vec3<f32>(seed)), pat_hash(c + vec3<f32>(3.1, seed, 7.7)), pat_hash(c + vec3<f32>(5.3, 1.9, seed)));
                    let dist = length(c + h - q);
                    if (dist < d1) {
                        d2 = d1;
                        d1 = dist;
                        shade = pat_hash(c + vec3<f32>(9.4, seed, 2.2));
                    } else if (dist < d2) {
                        d2 = dist;
                    }
                }
            }
        }
        var c = mix(base, i.pc.rgb, pow(shade, 1.5) * contrast);
        c = c * (0.88 + 0.12 * smoothstep(0.0, 0.05, d2 - d1));
        // Each flake lies a little askew, so its fibres do too.
        let tilt = (shade - 0.5) * 0.7;
        let along = f.x * cos(tilt) + f.y * sin(tilt);
        let across = f.y * cos(tilt) - f.x * sin(tilt);
        let fibre = pat_noise(vec3<f32>(along * 0.05, across * 0.9, f.z * 0.9) + vec3<f32>(shade * 40.0));
        return clamp(c * (1.0 + (fibre - 0.5) * 0.2), vec3<f32>(0.0), vec3<f32>(1.0));
    }
    if (kind == 2u) {
        // Brushed: streaks along the axis, long and thin. Real brushing is finer than a pixel at
        // most zooms, so the streaks widen to about two pixels instead of fading away.
        let w = max(s, foot * 2.5);
        let n = pat_fbm(vec3<f32>(f.x / (w * 40.0), f.y / w, f.z / w) + vec3<f32>(seed * 13.0));
        let streak = (n - 0.5) * contrast;
        return clamp(base * (1.0 + streak * 0.6) + vec3<f32>(streak * 0.05), vec3<f32>(0.0), vec3<f32>(1.0));
    }
    if (kind == 3u) {
        // Blasted, anodized or cast: an even, fine speckle, at least a pixel and a half across.
        let q = (i.wp - i.org) / max(s, foot * 1.5);
        let n = pat_noise(q + vec3<f32>(seed * 11.0)) * 0.65 + pat_noise(q * 0.27) * 0.35;
        return clamp(base * (1.0 + (n - 0.5) * contrast * 0.5), vec3<f32>(0.0), vec3<f32>(1.0));
    }
    if (kind == 4u) {
        // Mottled: patches of the second colour (patina, rust, wear), with a little grit.
        let q = (i.wp - i.org) / s;
        let n = pat_fbm(q + vec3<f32>(seed * 23.0));
        let edge = 0.62 - 0.3 * contrast;
        let blot = smoothstep(edge - 0.06, edge + 0.06, n);
        let grit = (pat_noise(q * 9.0) - 0.5) * 0.12 * pat_keep(s * 0.11, foot);
        return clamp(mix(base, i.pc.rgb, blot) * (1.0 + grit), vec3<f32>(0.0), vec3<f32>(1.0));
    }
    if (kind == 5u) {
        // Speckled: grains of the second colour (aggregate, granite crystals, terrazzo), lighter
        // flecks among them and a fine grit; `contrast` sets how many grains there are. Grains
        // stay at least about a pixel across.
        let q = (i.wp - i.org) / max(s, foot * 1.2);
        // Two octaves, so grains are small and irregular; the threshold keeps them apart (much
        // lower and they merge into patches).
        let a = pat_noise(q + vec3<f32>(seed * 19.0)) * 0.65 + pat_noise(q * 2.7 + vec3<f32>(9.1)) * 0.35;
        let b = pat_noise(q * 2.3 + vec3<f32>(5.2, seed * 7.0, 1.3));
        let edge = 0.66 - 0.16 * contrast;
        let dark = smoothstep(edge, edge + 0.035, a);
        let light = smoothstep(0.8, 0.86, b) * 0.6 * (1.0 - dark);
        var c = mix(base, i.pc.rgb, dark);
        c = mix(c, min(base * 1.25 + vec3<f32>(0.06), vec3<f32>(1.0)), light);
        let grit = (pat_noise(q * 6.0) - 0.5) * 0.1;
        return clamp(c * (1.0 + grit), vec3<f32>(0.0), vec3<f32>(1.0));
    }
    // Veined: thin winding veins of the second colour (marble), with finer veins between them
    // and a cloudy ground; a vein stays at least about a pixel wide.
    let q = f / s;
    let warp = pat_fbm(q * 0.4 + vec3<f32>(seed * 29.0)) * 2.5;
    let t = q.y * 0.8 + q.z * 0.5 + q.x * 0.25 + warp;
    let d = min(fract(t), 1.0 - fract(t)) * 2.0;
    let width = 0.05 + foot / s * 1.5;
    let vein = 1.0 - smoothstep(0.0, width, d);
    let t2 = t * 3.1 + pat_fbm(q * 1.3 + vec3<f32>(3.7)) * 2.0;
    let d2 = min(fract(t2), 1.0 - fract(t2)) * 2.0;
    let fine = (1.0 - smoothstep(0.0, width * 1.6, d2)) * 0.45 * pat_keep(s * 0.3, foot);
    let cloud = 0.95 + 0.1 * pat_fbm(q * 0.3 + vec3<f32>(11.0));
    let c = mix(base * cloud, i.pc.rgb, clamp(vein + fine, 0.0, 1.0) * contrast);
    return clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn shade(i: TOut) -> vec4<f32> {
    return lit(i, 1.0, 0.0);
}

fn lit(i: TOut, visibility: f32, foot: f32) -> vec4<f32> {
    // A zero normal marks flat colour (selection fills, translucent planes).
    if (length(i.n) < 0.5) {
        return out_color(i.c);
    }
    let v = normalize(u.back.xyz);
    var n = normalize(i.n);
    if (dot(n, v) < 0.0) { n = -n; }
    let key = normalize(v * 0.8 + vec3<f32>(-0.3, 0.2, 0.6));
    let diff = max(dot(n, key), 0.0);
    let fill = max(dot(n, v), 0.0);
    let h = normalize(key + v);
    let spec = pow(max(dot(n, h), 0.0), 40.0) * 0.25;
    let k = 0.42 + 0.38 * diff + 0.25 * fill - (0.42 + 0.25 * fill) * (1.0 - visibility);
    let base = pattern_color(i, foot);
    return out_color(vec4<f32>(min(base * k + vec3<f32>(spec), vec3<f32>(1.0)), i.c.a));
}

// Lines, decals and ghosts are drawn at their own depth: opaque faces are pushed back by their
// slope instead (`FACE_BIAS`, #1434), so what lies on a face draws over it at any angle.

struct LOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) c: vec4<f32>,
    @location(1) wp: vec3<f32>,
};

@vertex
fn vs_line(@builtin(vertex_index) vi: u32, @location(0) a: vec3<f32>, @location(1) b: vec3<f32>, @location(2) c: vec4<f32>, @location(3) w: f32) -> LOut {
    var pa = a;
    var pb = b;
    var ca = u.vp * vec4<f32>(a, 1.0);
    var cb = u.vp * vec4<f32>(b, 1.0);
    // An end behind the near plane (clip z < 0) has no screen point: its divide flips the
    // line or drops it whole (ground grid lines running past the eye at a low angle). Cut
    // the segment at the plane instead; clip space is linear in the world point.
    let za = near_z(ca);
    let zb = near_z(cb);
    if (za < 0.0 && zb > 0.0) {
        let k = za / (za - zb);
        ca = mix(ca, cb, k);
        pa = mix(a, b, k);
    } else if (zb < 0.0 && za > 0.0) {
        let k = zb / (zb - za);
        cb = mix(cb, ca, k);
        pb = mix(b, a, k);
    }
    let half = u.screen.xy * 0.5;
    let sa = ca.xy / max(ca.w, 1e-6) * half;
    let sb = cb.xy / max(cb.w, 1e-6) * half;
    var dir = sb - sa;
    let len = length(dir);
    if (len < 1e-6) { dir = vec2<f32>(1.0, 0.0); } else { dir = dir / len; }
    let nrm = vec2<f32>(-dir.y, dir.x);
    // Two triangles: corners (t, side).
    var ts = array<f32, 6>(0.0, 1.0, 1.0, 0.0, 1.0, 0.0);
    var ss = array<f32, 6>(-1.0, -1.0, 1.0, -1.0, 1.0, 1.0);
    let t = ts[vi];
    let s = ss[vi];
    let base = select(ca, cb, t > 0.5);
    // A line thinner than a pixel is drawn a pixel wide, faded by the width it lacks: a narrower
    // quad covers no pixel centre when it falls between two rows, so without multisampling thin
    // lines (the grid's) came and went as the view zoomed (#2823).
    let wd = max(w, 1.0);
    let off = nrm * s * wd * 0.5 + dir * (t * 2.0 - 1.0) * wd * 0.5;
    var o: LOut;
    o.pos = vec4<f32>(base.xy + off / half * base.w, base.z, base.w);
    o.c = vec4<f32>(c.rgb, c.a * (w / wd));
    o.wp = select(pa, pb, t > 0.5);
    return o;
}

@fragment
fn fs_line(i: LOut) -> @location(0) vec4<f32> {
    if (clipped(i.wp)) { discard; }
    return out_color(i.c);
}

struct IOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) a: f32,
    @location(2) wp: vec3<f32>,
};

@group(1) @binding(0) var img_t: texture_2d<f32>;
@group(1) @binding(1) var img_s: sampler;

@vertex
fn vs_img(@location(0) p: vec3<f32>, @location(1) uv: vec2<f32>, @location(2) a: f32) -> IOut {
    var o: IOut;
    // On a face it shows over it: the face is pushed back (`FACE_BIAS`).
    o.pos = u.vp * vec4<f32>(p, 1.0);
    o.uv = uv;
    o.a = a;
    o.wp = p;
    return o;
}

@fragment
fn fs_img(i: IOut) -> @location(0) vec4<f32> {
    let c = textureSample(img_t, img_s, i.uv);
    if (clipped(i.wp)) { discard; }
    return out_color(vec4<f32>(c.rgb, c.a * i.a));
}

/// Lines the section never cuts (the grid, highlights drawn on top).
@fragment
fn fs_line_all(i: LOut) -> @location(0) vec4<f32> {
    return out_color(i.c);
}
