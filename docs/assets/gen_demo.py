#!/usr/bin/env python3
"""Generate docs/assets/demo.svg: an 8 s looping CSS-animated Capcom demo.

Usage: python3 docs/assets/gen_demo.py docs/assets/demo.svg
"""
import math, random, sys

T = 8.0
W, H = 760, 340
css, body = [], []

def pct(t): return f"{max(0.0, min(100.0, t / T * 100)):.3f}%"

def kf(name, stops, timing=None):
    stops = sorted(stops, key=lambda s: s[0])
    if stops[0][0] > 0: stops.insert(0, (0, stops[0][1]))
    if stops[-1][0] < T: stops.append((T, stops[-1][1]))
    out = [f"@keyframes {name}{{"]
    for t, v in stops: out.append(f"{pct(t)}{{{v}}}")
    out.append("}")
    css.append("".join(out))
    tf = f" {timing}" if timing else ""
    css.append(f".{name}{{animation:{name} {T}s{tf} infinite}}")

def fade(name, t_in, t_out, d=0.12, lo=0, hi=1):
    kf(name, [(0, f"opacity:{lo}"), (t_in, f"opacity:{lo}"), (t_in + d, f"opacity:{hi}"),
              (t_out, f"opacity:{hi}"), (t_out + d, f"opacity:{lo}")])

FONT = "-apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif"
css.append(f"text{{font-family:{FONT}}}")
css.append(".fb{transform-box:fill-box;transform-origin:center}")

# --- timeline -------------------------------------------------------------
PRESS, RELEASE, DONE, TYPE0, END = 0.9, 4.0, 5.05, 5.15, 7.55

# Background card + stars.
body.append(f'''<defs>
<linearGradient id="sky" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#0a0f1f"/><stop offset="1" stop-color="#1a1f3d"/></linearGradient>
<linearGradient id="shine" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="#fff" stop-opacity="0"/><stop offset=".5" stop-color="#fff" stop-opacity=".22"/><stop offset="1" stop-color="#fff" stop-opacity="0"/></linearGradient>
<clipPath id="pillclip"><rect x="-84" y="-24" width="168" height="48" rx="24"/></clipPath>
<filter id="shadow" x="-30%" y="-30%" width="160%" height="180%"><feDropShadow dx="0" dy="6" stdDeviation="8" flood-color="#000" flood-opacity=".45"/></filter>
</defs>
<rect width="{W}" height="{H}" rx="18" fill="url(#sky)"/>''')
random.seed(7)
for i in range(38):
    x, y = random.uniform(10, W - 10), random.uniform(8, H - 8)
    if 40 < x < 720 and 18 < y < 200: continue  # behind the window
    r = random.choice([0.6, 0.8, 1.0, 1.3])
    dur = random.uniform(2.5, 5.5)
    delay = random.uniform(0, 4)
    body.append(f'<circle cx="{x:.1f}" cy="{y:.1f}" r="{r}" fill="#fff" style="opacity:.5;animation:tw {dur:.2f}s {delay:.2f}s ease-in-out infinite alternate"/>')
css.append("@keyframes tw{from{opacity:.15}to{opacity:.85}}")

# App window.
body.append('''<g>
<rect x="40" y="18" width="680" height="180" rx="14" fill="#fbfbfd"/>
<path d="M40 32a14 14 0 0 1 14-14h652a14 14 0 0 1 14 14v18H40z" fill="#ececf1"/>
<circle cx="62" cy="34" r="6" fill="#ff5f57"/><circle cx="82" cy="34" r="6" fill="#febc2e"/><circle cx="102" cy="34" r="6" fill="#28c840"/>
<text x="380" y="39" text-anchor="middle" font-size="13" font-weight="600" fill="#6e6e78">New Message</text>
<text x="70" y="80" font-size="15" fill="#8a8a94">To: <tspan fill="#2f6fe4">Mission Control</tspan></text>
<line x1="70" y1="94" x2="690" y2="94" stroke="#e4e4ea"/>
</g>''')

# Caret at the start of the message line (blinks until the paste).
TY = 140
blink = []
t = 0.0
while t < TYPE0:
    blink += [(t, "opacity:1"), (t + 0.5, "opacity:1"), (t + 0.501, "opacity:0"), (t + 0.999, "opacity:0")]
    t += 1.0
blink = [b for b in blink if b[0] < TYPE0 - 0.05] + [(TYPE0 - 0.05, "opacity:0"), (T, "opacity:0")]
kf("c0", blink)
body.append(f'<rect class="c0" x="70" y="{TY-19}" width="2" height="25" rx="1" fill="#2f6fe4"/>')

# Cleaned text, typed in char by char; caret rides along at the end.
clean = "Let’s meet at 3."
spans = []
for i, ch in enumerate(clean):
    ti = TYPE0 + i * 0.035
    kf(f"k{i}", [(0, "opacity:0"), (ti, "opacity:0"), (ti + 0.01, "opacity:1"), (END, "opacity:1"), (END + 0.25, "opacity:0")])
    spans.append(f'<tspan class="k{i}">{ch}</tspan>')
t_end = TYPE0 + len(clean) * 0.035
blink = [(0, "opacity:0"), (t_end, "opacity:0"), (t_end + 0.01, "opacity:1")]
t = t_end
while t < END:
    blink += [(t + 0.5, "opacity:1"), (t + 0.501, "opacity:0"), (t + 0.999, "opacity:0"), (t + 1.0, "opacity:1")]
    t += 1.0
blink = [b for b in blink if b[0] < END] + [(END, "opacity:0")]
kf("c1", blink)
spans.append('<tspan class="c1" fill="#2f6fe4" font-weight="300">|</tspan>')
body.append(f'<text x="70" y="{TY}" font-size="24" fill="#1d1d24">{"".join(spans)}</text>')

# Raw speech, word by word, above the pill. Fillers fade during processing.
words = [("um", 1), ("so", 1), ("let’s", 0), ("uh", 1), ("meet", 0), ("at", 0), ("2,", 1), ("actually", 1), ("3", 0)]
spans = []
for i, (wd, filler) in enumerate(words):
    ti = 1.35 + i * 0.29
    stops = [(0, "opacity:0;fill:#c9cff0"), (ti, "opacity:0;fill:#c9cff0"), (ti + 0.15, "opacity:1;fill:#c9cff0"),
             (RELEASE + 0.1, "opacity:1;fill:#c9cff0")]
    if filler:
        stops += [(RELEASE + 0.45 + 0.06 * i, "opacity:.28;fill:#ff8a8a"), (DONE, "opacity:.28;fill:#ff8a8a"), (DONE + 0.3, "opacity:0;fill:#ff8a8a")]
    else:
        stops += [(RELEASE + 0.45, "opacity:1;fill:#ffffff"), (DONE, "opacity:1;fill:#ffffff"), (DONE + 0.3, "opacity:0;fill:#ffffff")]
    kf(f"w{i}", stops)
    sep = " " if i else ""
    spans.append(f'{sep}<tspan class="w{i}">{wd}</tspan>')
body.append(f'<text x="380" y="240" text-anchor="middle" font-size="19" font-style="italic">“{"".join(spans)}”</text>'.replace("“<", "<").replace(">”", ">"))

# The pill (matches the real one x1.5: 112x32 -> 168x48, 14 bars 2.6/3.2 -> 3.9/4.8).
PX, PY = 380, 292
kf("pill", [(0, "opacity:0;transform:scale(.82)"), (PRESS + 0.08, "opacity:0;transform:scale(.82)"),
            (PRESS + 0.26, "opacity:1;transform:scale(1)"), (DONE, "opacity:1;transform:scale(1)"),
            (DONE + 0.24, "opacity:0;transform:scale(.9)")])
BARS, BW, BG, HMIN, HMAX = 14, 3.9, 4.8, 4.5, 33.0
span_w = BARS * BW + (BARS - 1) * BG
bars = []
rnd = random.Random(3)
for i in range(BARS):
    env = 0.45 + 0.55 * math.sin(math.pi * (i + 0.5) / BARS)
    stops = []
    w_i, ph_i = 7.0 + 9.0 * rnd.random(), math.tau * rnd.random()
    t = 0.0
    step = 0.08
    while t <= T + 1e-9:
        if PRESS + 0.1 <= t < RELEASE:
            u = t - PRESS
            syl = 0.5 + 0.5 * abs(math.sin(u * math.pi * 3.4))
            wob = 0.62 + 0.38 * math.sin(u * w_i + ph_i) * math.cos(u * w_i * 0.37 + ph_i * 1.7)
            roll = 0.85 + 0.15 * math.sin(u * 4.0 - i * 0.6)
            a = min(1.0, syl * env * wob * roll * 1.35 * (0.85 + 0.15 * rnd.random()))
            h, o = HMIN + (HMAX - HMIN) * a, 1.0
        elif RELEASE <= t < DONE:
            w = (0.5 + 0.5 * math.sin((t - RELEASE) * 6.0 * 1.6 - i * 0.55)) ** 2
            h, o = HMIN + 5.0 * w, 0.35 + 0.6 * w
        else:
            h, o = HMIN, 1.0
        stops.append((t, f"transform:scaleY({h / HMAX:.3f});opacity:{o:.2f}"))
        t = round(t + step, 4)
    kf(f"b{i}", stops, "linear")
    x = -span_w / 2 + i * (BW + BG)
    bars.append(f'<rect class="b{i} fb" x="{x:.2f}" y="{-HMAX/2}" width="{BW}" height="{HMAX}" rx="{BW/2}" fill="#fff"/>')
kf("shim", [(0, "transform:translateX(-200px)"), (RELEASE, "transform:translateX(-200px)"),
            (RELEASE + 0.5, "transform:translateX(200px)"), (RELEASE + 0.501, "transform:translateX(-200px)"),
            (DONE - 0.05, "transform:translateX(200px)"), (DONE, "transform:translateX(200px)")], "linear")
body.append(f'''<g transform="translate({PX} {PY})"><g class="pill fb" filter="url(#shadow)">
<rect x="-84" y="-24" width="168" height="48" rx="24" fill="#0e0e10" fill-opacity=".94" stroke="#fff" stroke-opacity=".14"/>
{"".join(bars)}
<g clip-path="url(#pillclip)"><rect class="shim" x="-60" y="-24" width="60" height="48" fill="url(#shine)"/></g>
</g></g>''')

# Key cap (bottom-left): pressed while dictating.
kf("key", [(0, "transform:translateY(0)"), (PRESS, "transform:translateY(0)"), (PRESS + 0.06, "transform:translateY(3px)"),
           (RELEASE, "transform:translateY(3px)"), (RELEASE + 0.06, "transform:translateY(0)")])
kf("keyglow", [(0, "fill:#2a3156"), (PRESS, "fill:#2a3156"), (PRESS + 0.06, "fill:#4b5bd6"), (RELEASE, "fill:#4b5bd6"), (RELEASE + 0.06, "fill:#2a3156")])
body.append('''<g transform="translate(46 262)"><rect x="0" y="6" width="74" height="50" rx="9" fill="#05070f"/>
<g class="key"><rect class="keyglow" x="0" y="0" width="74" height="50" rx="9" stroke="#6b77c9" stroke-opacity=".5"/>
<text x="10" y="21" font-size="15" fill="#fff">⌃</text><text x="64" y="41" text-anchor="end" font-size="11" fill="#c9cff0">control</text></g></g>''')

# Status caption (bottom-right).
caps = [("Hold right ⌃ and talk", 0, PRESS), ("Listening…", PRESS, RELEASE), ("Cleaning up…", RELEASE, DONE), ("Pasted ✓  0.6 s", DONE + 0.1, END + 0.2)]
for i, (txt, a, b) in enumerate(caps):
    if a == 0:
        kf(f"s{i}", [(0, "opacity:1"), (b, "opacity:1"), (b + 0.1, "opacity:0"), (END + 0.3, "opacity:0"), (T - 0.1, "opacity:1")])
    else:
        fade(f"s{i}", a, b, 0.1)
    body.append(f'<text class="s{i}" x="714" y="298" text-anchor="end" font-size="15" font-weight="500" fill="#c9cff0">{txt}</text>')

svg = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" role="img" aria-label="Capcom demo: hold the key, speak, and cleaned-up text is pasted at the cursor">
<title>Capcom: hold a key, talk, get clean text</title>
<style>{"".join(css)}</style>
{chr(10).join(body)}
</svg>
'''
open(sys.argv[1], "w").write(svg)
print(len(svg), "bytes")
