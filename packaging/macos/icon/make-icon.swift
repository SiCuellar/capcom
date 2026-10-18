// Capcom "Orbit" mark: a solid dot (the voice) inside a tilted orbit ring whose arc turns into a
// sound wave. Pure Core Graphics, vector-quality, no assets.
//
// Usage:
//   swift make-icon.swift iconset <out.iconset>      app icon, all sizes 16...1024 (see make-icon.sh)
//   swift make-icon.swift glyph   <dir>              menu-bar template PNGs (18 pt @1x/@2x + processing)
//   swift make-icon.swift explore <sheet.png>        exploration contact sheet (all variants)
//   swift make-icon.swift final   <showcase.png>     showcase of the shipped icon + menu-bar glyph
import AppKit

// MARK: - Model

enum Wave {
    /// The ring itself oscillates (along its normal) over an arc segment.
    case sine(center: CGFloat, span: CGFloat, cycles: CGFloat, amp: CGFloat)
    /// The ring opens over an arc segment and the gap is filled with equalizer bars.
    case bars(center: CGFloat, span: CGFloat, heights: [CGFloat], vertical: Bool)
    /// The ring opens at its right tip; the gap holds vertical bars standing on the major axis
    /// (`xs` along the axis), so the orbit "speaks" outward through its tip.
    case axis(span: CGFloat, xs: [CGFloat], heights: [CGFloat])
}

struct Palette {
    var top: NSColor
    var bottom: NSColor
    var mark: NSColor
    var glow: NSColor? = nil
}

struct Variant {
    var id: String
    var desc: String
    var shift: CGFloat = 0    // optical horizontal shift of the whole mark (units)
    var a: CGFloat            // ellipse semi-axis (unit = half the mark box)
    var b: CGFloat
    var tilt: CGFloat         // degrees, counter-clockwise
    var dot: CGFloat          // dot radius
    var dotOffset = CGPoint.zero
    var stroke: CGFloat       // ring stroke width
    var wave: Wave
    var palette: Palette
}

func rgb(_ r: CGFloat, _ g: CGFloat, _ b: CGFloat, _ a: CGFloat = 1) -> NSColor {
    NSColor(srgbRed: r / 255, green: g / 255, blue: b / 255, alpha: a)
}

let navy = Palette(top: rgb(28, 34, 72), bottom: rgb(4, 5, 14), mark: .white, glow: rgb(120, 140, 255, 0.18))
let paper = Palette(top: rgb(250, 247, 240), bottom: rgb(236, 231, 220), mark: rgb(20, 20, 22))
let lilac = Palette(top: rgb(196, 181, 253), bottom: rgb(139, 116, 240), mark: .white)
let violet = Palette(top: rgb(124, 77, 255), bottom: rgb(64, 28, 190), mark: .white)
let graphite = Palette(top: rgb(58, 60, 66), bottom: rgb(22, 23, 26), mark: rgb(245, 245, 247))

// MARK: - Geometry

/// Ellipse sampled by arc length: points, outward normals, cumulative length.
struct Orbit {
    var pts: [CGPoint] = []
    var nrm: [CGPoint] = []
    var len: [CGFloat] = []
    var total: CGFloat { len.last ?? 0 }

    init(a: CGFloat, b: CGFloat, tilt: CGFloat, n: Int = 2400) {
        let th = tilt * .pi / 180, c = cos(th), s = sin(th)
        var acc: CGFloat = 0
        for i in 0...n {
            let t = CGFloat(i) / CGFloat(n) * 2 * .pi
            let x = a * cos(t), y = b * sin(t)
            let p = CGPoint(x: x * c - y * s, y: x * s + y * c)
            var nx = b * cos(t), ny = a * sin(t)
            let l = hypot(nx, ny); nx /= l; ny /= l
            if let last = pts.last { acc += hypot(p.x - last.x, p.y - last.y) }
            pts.append(p); nrm.append(CGPoint(x: nx * c - ny * s, y: nx * s + ny * c)); len.append(acc)
        }
    }

    /// Arc length at ellipse parameter angle `deg` (degrees).
    func at(_ deg: CGFloat) -> CGFloat {
        var t = deg.truncatingRemainder(dividingBy: 360); if t < 0 { t += 360 }
        let i = Int(t / 360 * CGFloat(pts.count - 1))
        return len[min(i, len.count - 1)]
    }

    /// Arc-length midpoint and half-length of the param-angle window center±span/2 (degrees).
    func window(_ center: CGFloat, _ span: CGFloat) -> (CGFloat, CGFloat) {
        let s0 = at(center - span / 2), s1 = at(center + span / 2)
        var d = s1 - s0; if d < 0 { d += total }
        return (s0 + d / 2, d / 2)
    }

    /// Point + normal at arc length `s` (wrapping).
    func sample(_ s: CGFloat) -> (CGPoint, CGPoint) {
        var s = s.truncatingRemainder(dividingBy: total); if s < 0 { s += total }
        var lo = 0, hi = len.count - 1
        while hi - lo > 1 { let m = (lo + hi) / 2; if len[m] < s { lo = m } else { hi = m } }
        return (pts[hi], nrm[hi])
    }
}

/// Draws the mark centered at `c`, `u` px per unit, in `color`. `px` = final icon size, used to
/// simplify the mark for tiny renders (thicker stroke, fewer wave cycles / bars).
func drawMark(_ ctx: CGContext, _ v: Variant, c: CGPoint, u: CGFloat, color: CGColor, px: CGFloat) {
    var v = v
    let small = px <= 20, medium = px <= 40
    var lw = v.stroke * u
    if small || medium {
        // Dedicated small-size drawing: rounder/less tilted orbit (more room for the dot), stroke
        // set in real pixels, fewer wave elements.
        lw = small ? 1.3 * px / 16 : 1.8 * px / 32
        switch v.wave {
        case let .sine(center, span, cycles, amp):
            v.wave = .sine(center: center, span: span, cycles: small ? min(cycles, 1.5) : min(cycles, 2.5), amp: amp * (small ? 1.25 : 1.1))
        case let .bars(center, span, heights, vertical):
            if small && heights.count > 3 {
                let mid = heights.count / 2
                v.wave = .bars(center: center, span: span * 0.8, heights: [heights[mid - 1], heights[mid], heights[mid + 1]], vertical: vertical)
            }
        case let .axis(span, xs, heights):
            let m = xs.count / 2
            v.b = max(v.b, v.a * 0.66)
            v.tilt *= 0.7
            if small {
                // One confident bar at the tip.
                v.wave = .axis(span: span * 1.15, xs: [xs[m] + 0.16], heights: [heights[m] * 1.1])
            } else {
                v.wave = .axis(span: span * 1.25, xs: [xs[m] - 0.04, xs[m] + 0.2], heights: [heights[m] * 1.05, heights[m + 1] * 1.15])
            }
        }
        v.dot = max(v.dot, small ? 0.23 : 0.21)
    }
    let orbit = Orbit(a: v.a, b: v.b, tilt: v.tilt)
    let c = CGPoint(x: c.x + v.shift * u, y: c.y)
    func P(_ p: CGPoint) -> CGPoint { CGPoint(x: c.x + p.x * u, y: c.y + p.y * u) }

    ctx.saveGState()
    ctx.setStrokeColor(color); ctx.setFillColor(color)
    ctx.setLineWidth(lw); ctx.setLineCap(.round); ctx.setLineJoin(.round)

    switch v.wave {
    case let .sine(center, span, cycles, amp):
        let (sc, half) = orbit.window(center, span)
        let steps = 1600
        let path = CGMutablePath()
        for i in 0...steps {
            let s = sc + CGFloat(i) / CGFloat(steps) * orbit.total
            var d = s - sc; if d > orbit.total / 2 { d -= orbit.total }
            var off: CGFloat = 0
            if abs(d) < half {
                let x = d / half                         // -1...1
                let w = pow(cos(x * .pi / 2), 1.2)       // smooth envelope, 0 at ends
                off = amp * w * sin(.pi * cycles * (x + 1))
            }
            let (p, n) = orbit.sample(s)
            let q = P(CGPoint(x: p.x + n.x * off, y: p.y + n.y * off))
            if i == 0 { path.move(to: q) } else { path.addLine(to: q) }
        }
        path.closeSubpath()
        ctx.addPath(path); ctx.strokePath()
    case let .bars(center, span, heights, vertical):
        let (sc, half) = orbit.window(center, span)
        // Ring arc outside the gap.
        let path = CGMutablePath()
        let steps = 1200
        for i in 0...steps {
            let s = sc + half + CGFloat(i) / CGFloat(steps) * (orbit.total - 2 * half)
            let q = P(orbit.sample(s).0)
            if i == 0 { path.move(to: q) } else { path.addLine(to: q) }
        }
        ctx.addPath(path); ctx.strokePath()
        // Bars inside the gap.
        let n = heights.count
        for (i, h) in heights.enumerated() {
            let f = (CGFloat(i) + 1) / CGFloat(n + 1)
            let (p, nn) = orbit.sample(sc - half + f * 2 * half)
            let dir = vertical ? CGPoint(x: 0, y: 1) : nn
            let hh = max(h * u / 2, 0.001)
            let p0 = P(p)
            ctx.move(to: CGPoint(x: p0.x - dir.x * hh, y: p0.y - dir.y * hh))
            ctx.addLine(to: CGPoint(x: p0.x + dir.x * hh, y: p0.y + dir.y * hh))
            ctx.strokePath()
        }

    case let .axis(span, xs, heights):
        let (sc, half) = orbit.window(0, span)
        let path = CGMutablePath()
        let steps = 1200
        for i in 0...steps {
            let s = sc + half + CGFloat(i) / CGFloat(steps) * (orbit.total - 2 * half)
            let q = P(orbit.sample(s).0)
            if i == 0 { path.move(to: q) } else { path.addLine(to: q) }
        }
        ctx.addPath(path); ctx.strokePath()
        let th = v.tilt * .pi / 180
        for (x, h) in zip(xs, heights) {
            let p0 = P(CGPoint(x: x * cos(th), y: x * sin(th)))
            let hh = h * u / 2
            ctx.move(to: CGPoint(x: p0.x, y: p0.y - hh)); ctx.addLine(to: CGPoint(x: p0.x, y: p0.y + hh))
            ctx.strokePath()
        }
    }

    // The voice: a solid dot.
    let dc = P(v.dotOffset), r = v.dot * u
    ctx.fillEllipse(in: CGRect(x: dc.x - r, y: dc.y - r, width: 2 * r, height: 2 * r))
    ctx.restoreGState()
}

/// Apple-style continuous-corner tile (superellipse), 824/1024 of the canvas.
func tilePath(_ rect: CGRect) -> CGPath {
    let path = CGMutablePath()
    let n: CGFloat = 5.0, steps = 720
    let cx = rect.midX, cy = rect.midY, rx = rect.width / 2, ry = rect.height / 2
    for i in 0..<steps {
        let t = CGFloat(i) / CGFloat(steps) * 2 * .pi
        let ct = cos(t), st = sin(t)
        let x = cx + rx * (ct < 0 ? -1 : 1) * pow(abs(ct), 2 / n)
        let y = cy + ry * (st < 0 ? -1 : 1) * pow(abs(st), 2 / n)
        if i == 0 { path.move(to: CGPoint(x: x, y: y)) } else { path.addLine(to: CGPoint(x: x, y: y)) }
    }
    path.closeSubpath()
    return path
}

func drawTile(_ ctx: CGContext, s: CGFloat, p: Palette) -> CGRect {
    let inset = s * 100 / 1024
    let body = CGRect(x: inset, y: inset, width: s - 2 * inset, height: s - 2 * inset)
    let path = tilePath(body)
    // Soft drop shadow (Apple grid leaves room for it).
    if s >= 64 {
        ctx.saveGState()
        ctx.setShadow(offset: CGSize(width: 0, height: -s * 0.01), blur: s * 0.025, color: NSColor.black.withAlphaComponent(0.3).cgColor)
        ctx.addPath(path); ctx.setFillColor(p.bottom.cgColor); ctx.fillPath()
        ctx.restoreGState()
    }
    ctx.saveGState()
    ctx.addPath(path); ctx.clip()
    let cs = CGColorSpace(name: CGColorSpace.sRGB)!
    let g = CGGradient(colorsSpace: cs, colors: [p.top.cgColor, p.bottom.cgColor] as CFArray, locations: [0, 1])!
    ctx.drawLinearGradient(g, start: CGPoint(x: s / 2, y: body.maxY), end: CGPoint(x: s / 2, y: body.minY), options: [])
    if let glow = p.glow {
        let rg = CGGradient(colorsSpace: cs, colors: [glow.cgColor, glow.withAlphaComponent(0).cgColor] as CFArray, locations: [0, 1])!
        let gc = CGPoint(x: s * 0.5, y: s * 0.56)
        ctx.drawRadialGradient(rg, startCenter: gc, startRadius: 0, endCenter: gc, endRadius: s * 0.42, options: [])
    }
    ctx.restoreGState()
    // Hairline top highlight for a hint of depth.
    if s >= 128 {
        ctx.saveGState()
        ctx.addPath(path); ctx.clip()
        ctx.addPath(path)
        ctx.setStrokeColor(NSColor.white.withAlphaComponent(0.10).cgColor)
        ctx.setLineWidth(s * 0.006)
        ctx.strokePath()
        ctx.restoreGState()
    }
    return body
}

func bitmap(_ w: Int, _ h: Int) -> CGContext {
    let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
                        space: CGColorSpace(name: CGColorSpace.sRGB)!,
                        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.setShouldAntialias(true); ctx.setAllowsAntialiasing(true)
    ctx.interpolationQuality = .high
    return ctx
}

func icon(_ v: Variant, _ px: Int) -> CGImage {
    let s = CGFloat(px)
    let ctx = bitmap(px, px)
    let body = drawTile(ctx, s: s, p: v.palette)
    // Mark box = ~80% of the tile body: unit = 0.40 * body width.
    let u = body.width * 0.40
    drawMark(ctx, v, c: CGPoint(x: body.midX, y: body.midY), u: u, color: v.palette.mark.cgColor, px: s)
    return ctx.makeImage()!
}

func png(_ img: CGImage, _ path: String) {
    let rep = NSBitmapImageRep(cgImage: img)
    try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: path))
}

// MARK: - Variants

let variants: [Variant] = [
    Variant(id: "V1", desc: "Navy tilted voice-tip", shift: -0.1, a: 0.80, b: 0.48, tilt: 16, dot: 0.17, stroke: 0.095,
            wave: .axis(span: 104, xs: [0.60, 0.80, 1.0], heights: [0.36, 0.66, 0.32]), palette: navy),
    Variant(id: "V2", desc: "Lilac flat voice-tip", shift: -0.12, a: 0.78, b: 0.50, tilt: 0, dot: 0.16, stroke: 0.09,
            wave: .axis(span: 96, xs: [0.56, 0.74, 0.92, 1.10], heights: [0.40, 0.70, 0.46, 0.22]), palette: lilac),
    Variant(id: "V3", desc: "Paper C-ring voice", shift: -0.1, a: 0.74, b: 0.74, tilt: 0, dot: 0.2, stroke: 0.1,
            wave: .axis(span: 84, xs: [0.56, 0.78, 1.0], heights: [0.42, 0.72, 0.34]), palette: paper),
    Variant(id: "V4", desc: "Violet steep orbit", shift: -0.08, a: 0.82, b: 0.44, tilt: 28, dot: 0.16, stroke: 0.09,
            wave: .axis(span: 90, xs: [0.64, 0.84, 1.03], heights: [0.34, 0.6, 0.28]), palette: violet),
    Variant(id: "V5", desc: "Graphite Kepler focus", a: 0.90, b: 0.56, tilt: 20, dot: 0.17, dotOffset: CGPoint(x: -0.30, y: -0.06), stroke: 0.09,
            wave: .bars(center: -20, span: 80, heights: [0.2, 0.44, 0.2], vertical: true), palette: graphite),
    Variant(id: "V6", desc: "Monochrome tilted voice-tip", shift: -0.1, a: 0.80, b: 0.48, tilt: 16, dot: 0.16, stroke: 0.09,
            wave: .axis(span: 92, xs: [0.62, 0.81, 1.0], heights: [0.36, 0.62, 0.30]),
            palette: Palette(top: rgb(255, 255, 255), bottom: rgb(226, 228, 233), mark: rgb(16, 16, 20))),
]

/// The shipped mark.
var primary: Variant { variants[0] }

// MARK: - Menu-bar glyph (template image: black on transparent)

enum GlyphKind { case idle, listening, processing }

/// 18 pt menu-bar glyph. idle: orbit + two voice bars (template, black); listening: taller bars in
/// system red (non-template); processing: the open orbit with its voice gone quiet (template).
func glyph(_ kind: GlyphKind, scale: Int) -> CGImage {
    let pt: CGFloat = 18, s = pt * CGFloat(scale)
    let ctx = bitmap(Int(s), Int(s))
    var v = primary
    v.a = 0.80; v.b = 0.52; v.tilt = 14; v.dot = 0.2; v.shift = -0.12
    v.stroke = 1.55 / 8.0   // ~1.55 pt at u = 8 pt
    switch kind {
    case .idle: v.wave = .axis(span: 120, xs: [0.78, 1.04], heights: [0.78, 0.44])
    case .listening: v.wave = .axis(span: 120, xs: [0.78, 1.04], heights: [0.96, 0.6])
    case .processing: v.wave = .axis(span: 120, xs: [], heights: [])
    }
    let u = s / 18 * 8
    let color = kind == .listening ? rgb(255, 59, 48).cgColor : NSColor.black.cgColor
    drawMark(ctx, v, c: CGPoint(x: s / 2, y: s / 2), u: u, color: color, px: 1024)
    return ctx.makeImage()!
}

// MARK: - Sheets

func label(_ text: String, at p: CGPoint, size: CGFloat, color: NSColor, weight: NSFont.Weight = .regular) {
    let attrs: [NSAttributedString.Key: Any] = [.font: NSFont.systemFont(ofSize: size, weight: weight), .foregroundColor: color]
    NSAttributedString(string: text, attributes: attrs).draw(at: p)
}

func withNS(_ ctx: CGContext, _ body: () -> Void) {
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(cgContext: ctx, flipped: false)
    body()
    NSGraphicsContext.restoreGraphicsState()
}

func exploreSheet(_ path: String) {
    let cols = 3, cellW = 620, cellH = 760
    let W = cols * cellW, H = ((variants.count + cols - 1) / cols) * cellH + 80
    let ctx = bitmap(W, H)
    ctx.setFillColor(rgb(242, 242, 245).cgColor); ctx.fill(CGRect(x: 0, y: 0, width: W, height: H))
    withNS(ctx) { label("Capcom — Orbit explorations", at: CGPoint(x: 40, y: CGFloat(H) - 60), size: 30, color: .black, weight: .semibold) }
    for (i, v) in variants.enumerated() {
        let col = i % cols, row = i / cols
        let x0 = CGFloat(col * cellW), y0 = CGFloat(H - 80 - (row + 1) * cellH)
        ctx.draw(icon(v, 512), in: CGRect(x: x0 + 54, y: y0 + 200, width: 512, height: 512))
        // Small sizes: actual pixels, then 4x nearest-neighbour zoom.
        let i32 = icon(v, 32), i16 = icon(v, 16)
        ctx.interpolationQuality = .none
        ctx.draw(i32, in: CGRect(x: x0 + 70, y: y0 + 120, width: 32, height: 32))
        ctx.draw(i16, in: CGRect(x: x0 + 120, y: y0 + 128, width: 16, height: 16))
        ctx.draw(i32, in: CGRect(x: x0 + 170, y: y0 + 72, width: 128, height: 128))
        ctx.draw(i16, in: CGRect(x: x0 + 320, y: y0 + 72, width: 64, height: 64))
        // 32 and 16 on a dark strip too.
        ctx.setFillColor(rgb(30, 30, 34).cgColor); ctx.fill(CGRect(x: x0 + 410, y: y0 + 100, width: 150, height: 60))
        ctx.draw(i32, in: CGRect(x: x0 + 425, y: y0 + 114, width: 32, height: 32))
        ctx.draw(i16, in: CGRect(x: x0 + 480, y: y0 + 122, width: 16, height: 16))
        ctx.interpolationQuality = .high
        withNS(ctx) {
            label(v.id, at: CGPoint(x: x0 + 54, y: y0 + 24), size: 26, color: .black, weight: .bold)
            label(v.desc, at: CGPoint(x: x0 + 110, y: y0 + 26), size: 24, color: .darkGray)
        }
    }
    png(ctx.makeImage()!, path)
}

func finalSheet(_ path: String) {
    let W = 1200, H = 760
    let ctx = bitmap(W, H)
    ctx.setFillColor(rgb(242, 242, 245).cgColor); ctx.fill(CGRect(x: 0, y: 0, width: W, height: H))
    ctx.draw(icon(primary, 512), in: CGRect(x: 40, y: 200, width: 512, height: 512))
    ctx.draw(icon(primary, 128), in: CGRect(x: 600, y: 584, width: 128, height: 128))
    ctx.draw(icon(primary, 64), in: CGRect(x: 760, y: 616, width: 64, height: 64))
    ctx.draw(icon(primary, 32), in: CGRect(x: 850, y: 632, width: 32, height: 32))
    ctx.draw(icon(primary, 16), in: CGRect(x: 905, y: 640, width: 16, height: 16))
    // Menu-bar strips (glyph at 2x = real Retina pixels, shown at 1:1 and zoomed), tinted like macOS does.
    func strip(_ y: CGFloat, bg: NSColor, fg: NSColor, title: String) {
        ctx.setFillColor(bg.cgColor); ctx.fill(CGRect(x: 600, y: y, width: 560, height: 120))
        var x: CGFloat = 630
        for kind in [GlyphKind.idle, .listening, .processing] {
            let g = glyph(kind, scale: 2)
            for rect in [CGRect(x: x, y: y + 42, width: 36, height: 36), CGRect(x: x + 44, y: y + 18, width: 84, height: 84)] {
                if kind == .listening { ctx.draw(g, in: rect); continue }
                ctx.saveGState(); ctx.clip(to: rect, mask: g); ctx.setFillColor(fg.cgColor); ctx.fill(rect); ctx.restoreGState()
            }
            x += 140
        }
        withNS(ctx) { label(title, at: CGPoint(x: 1060, y: y + 48), size: 18, color: fg) }
    }
    strip(400, bg: rgb(236, 236, 238), fg: rgb(0, 0, 0, 0.85), title: "light")
    strip(250, bg: rgb(36, 36, 40), fg: .white, title: "dark")
    withNS(ctx) {
        label("Capcom — Orbit", at: CGPoint(x: 40, y: 130), size: 34, color: .black, weight: .semibold)
        label("app icon 512 · 128 · 64 · 32 · 16      menu bar: idle · listening · processing (1:1 Retina + zoom)", at: CGPoint(x: 40, y: 90), size: 18, color: .darkGray)
    }
    png(ctx.makeImage()!, path)
}

// MARK: - Main

let args = CommandLine.arguments
let mode = args.count > 1 ? args[1] : "iconset"
let out = args.count > 2 ? args[2] : "Capcom.iconset"
switch mode {
case "iconset":
    try? FileManager.default.createDirectory(atPath: out, withIntermediateDirectories: true)
    for base in [16, 32, 128, 256, 512] {
        for scale in [1, 2] {
            let name = scale == 1 ? "icon_\(base)x\(base).png" : "icon_\(base)x\(base)@2x.png"
            png(icon(primary, base * scale), "\(out)/\(name)")
        }
    }
case "glyph":
    try? FileManager.default.createDirectory(atPath: out, withIntermediateDirectories: true)
    png(glyph(.idle, scale: 1), "\(out)/orbit-template.png")
    png(glyph(.idle, scale: 2), "\(out)/orbit-template@2x.png")
    png(glyph(.processing, scale: 1), "\(out)/orbit-processing-template.png")
    png(glyph(.processing, scale: 2), "\(out)/orbit-processing-template@2x.png")
    png(glyph(.listening, scale: 1), "\(out)/orbit-listening.png")
    png(glyph(.listening, scale: 2), "\(out)/orbit-listening@2x.png")
case "explore": exploreSheet(out)
case "zoom": // debug: tiny sizes of V1 and V3, nearest-neighbour zoom to 256
    let pick = [variants[0], variants[2]]
    let ctx = bitmap(4 * 270, 270)
    ctx.setFillColor(rgb(242, 242, 245).cgColor); ctx.fill(CGRect(x: 0, y: 0, width: 4 * 270, height: 270))
    ctx.interpolationQuality = .none
    for (i, v) in pick.enumerated() {
        ctx.draw(icon(v, 16), in: CGRect(x: i * 540 + 5, y: 5, width: 256, height: 256))
        ctx.draw(icon(v, 32), in: CGRect(x: i * 540 + 275, y: 5, width: 256, height: 256))
    }
    png(ctx.makeImage()!, out)
case "final": finalSheet(out)
default: print("unknown mode \(mode)"); exit(1)
}
print("wrote \(out)")
