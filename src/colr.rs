//! Emoji coloridos no formato COLRv1 (o Noto Color Emoji do Fedora e de
//! outras distros Linux vem so assim: Noto-COLRv1.ttf). O swash, que o
//! glyphon usa para rasterizar, so entende COLRv0 e bitmaps (CBDT/sbix):
//! nesses glifos ele cai no contorno comum, que e vazio, e o emoji saia em
//! branco. Aqui o grafo de pintura que o skrifa percorre vira uma imagem
//! RGBA, entregue ao glyphon como glifo customizado.
//!
//! Fontes COLRv0 (Segoe UI Emoji no Windows) seguem pelo swash: so passa
//! por aqui o glifo que nao tem versao COLRv0.

use skrifa::color::{Brush, ColorGlyphFormat, ColorPainter, ColorStop, CompositeMode, Extend, Transform};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::raw::types::BoundingBox;
use skrifa::{FontRef, GlyphId, MetadataProvider};
use zeno::{Command, Fill, Mask, Vector};

/// O glifo so tem pintura COLRv1 (o swash nao o desenharia).
pub fn is_v1_only(data: &[u8], index: u32, gid: u16) -> bool {
    let Ok(font) = FontRef::from_index(data, index) else { return false };
    let cg = font.color_glyphs();
    let g = GlyphId::new(gid as u32);
    cg.get_with_format(g, ColorGlyphFormat::ColrV0).is_none() && cg.get_with_format(g, ColorGlyphFormat::ColrV1).is_some()
}

/// A fonte tem tabela COLR de versao 1 (filtro barato antes de olhar glifo a glifo).
pub fn has_v1(data: &[u8], index: u32) -> bool {
    use skrifa::raw::TableProvider;
    FontRef::from_index(data, index).ok().and_then(|f| f.colr().ok()).is_some_and(|c| c.version() >= 1)
}

/// Caixa do glifo em px inteiros, relativa a origem na linha de base, com y
/// para baixo: (esquerda, topo, largura, altura). Usa a ClipBox do COLRv1;
/// sem ela, avanco x (ascendente..descendente).
pub fn bounds(data: &[u8], index: u32, gid: u16, px: f32) -> Option<(i32, i32, u32, u32)> {
    let font = FontRef::from_index(data, index).ok()?;
    let g = GlyphId::new(gid as u32);
    let glyph = font.color_glyphs().get_with_format(g, ColorGlyphFormat::ColrV1)?;
    let b = glyph.bounding_box(LocationRef::default(), Size::new(px)).unwrap_or_else(|| {
        let m = font.metrics(Size::new(px), LocationRef::default());
        let adv = font.glyph_metrics(Size::new(px), LocationRef::default()).advance_width(g).unwrap_or(px);
        BoundingBox { x_min: 0.0, y_min: m.descent, x_max: adv, y_max: m.ascent }
    });
    let (x0, x1, y0, y1) = (b.x_min.floor(), b.x_max.ceil(), b.y_min.floor(), b.y_max.ceil());
    let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
    (w > 0 && h > 0 && w < 2048 && h < 2048).then_some((x0 as i32, -(y1 as i32), w, h))
}

/// Rasteriza o glifo em RGBA (alfa nao pre-multiplicado, sRGB) de w x h,
/// com a origem do glifo em (-left, -top) da imagem. `fg` e a cor usada
/// quando a fonte pede a "cor do texto" (indice 0xFFFF da paleta).
pub fn rasterize(data: &[u8], index: u32, gid: u16, px: f32, left: i32, top: i32, w: u32, h: u32, fg: [u8; 4]) -> Option<Vec<u8>> {
    let font = FontRef::from_index(data, index).ok()?;
    let glyph = font.color_glyphs().get_with_format(GlyphId::new(gid as u32), ColorGlyphFormat::ColrV1)?;
    let upem = font.metrics(Size::unscaled(), LocationRef::default()).units_per_em.max(1) as f32;
    let s = px / upem;
    let palette: Vec<[f32; 4]> = font.color_palettes().get(0)
        .map(|p| p.colors().iter().map(|c| [c.red as f32 / 255.0, c.green as f32 / 255.0, c.blue as f32 / 255.0, c.alpha as f32 / 255.0]).collect())
        .unwrap_or_default();
    let (w, h) = (w as usize, h as usize);
    // Unidades da fonte (y para cima) -> pixels da imagem (y para baixo).
    let base = Transform { xx: s, yx: 0.0, xy: 0.0, yy: -s, dx: -left as f32, dy: -top as f32 };
    let mut p = Painter {
        font: &font, palette, fg: fg.map(|c| c as f32 / 255.0), w, h,
        xf: vec![base], clips: Vec::new(), layers: vec![(vec![[0.0; 4]; w * h], CompositeMode::SrcOver)],
    };
    glyph.paint(LocationRef::default(), &mut p).ok()?;
    let img = &p.layers[0].0;
    let mut out = Vec::with_capacity(w * h * 4);
    for c in img {
        let a = c[3].clamp(0.0, 1.0);
        let un = |v: f32| if a > 0.0 { (v / a).clamp(0.0, 1.0) } else { 0.0 };
        out.extend([un(c[0]), un(c[1]), un(c[2]), a].map(|v| (v * 255.0 + 0.5) as u8));
    }
    Some(out)
}

fn mul(a: &Transform, b: &Transform) -> Transform {
    Transform {
        xx: a.xx * b.xx + a.xy * b.yx,
        yx: a.yx * b.xx + a.yy * b.yx,
        xy: a.xx * b.xy + a.xy * b.yy,
        yy: a.yx * b.xy + a.yy * b.yy,
        dx: a.xx * b.dx + a.xy * b.dy + a.dx,
        dy: a.yx * b.dx + a.yy * b.dy + a.dy,
    }
}

fn apply(t: &Transform, x: f32, y: f32) -> (f32, f32) { (t.xx * x + t.xy * y + t.dx, t.yx * x + t.yy * y + t.dy) }

fn invert(t: &Transform) -> Option<Transform> {
    let det = t.xx * t.yy - t.xy * t.yx;
    if det.abs() < 1e-12 { return None; }
    let (xx, xy, yx, yy) = (t.yy / det, -t.xy / det, -t.yx / det, t.xx / det);
    Some(Transform { xx, yx, xy, yy, dx: -(xx * t.dx + xy * t.dy), dy: -(yx * t.dx + yy * t.dy) })
}

/// Caneta que ja leva o contorno para pixels da imagem.
struct Pen { t: Transform, cmds: Vec<Command> }

impl Pen {
    fn pt(&self, x: f32, y: f32) -> Vector { let (x, y) = apply(&self.t, x, y); Vector::new(x, y) }
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) { let p = self.pt(x, y); self.cmds.push(Command::MoveTo(p)); }
    fn line_to(&mut self, x: f32, y: f32) { let p = self.pt(x, y); self.cmds.push(Command::LineTo(p)); }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) { let (c, p) = (self.pt(cx, cy), self.pt(x, y)); self.cmds.push(Command::QuadTo(c, p)); }
    fn curve_to(&mut self, c0x: f32, c0y: f32, c1x: f32, c1y: f32, x: f32, y: f32) {
        let (a, b, p) = (self.pt(c0x, c0y), self.pt(c1x, c1y), self.pt(x, y));
        self.cmds.push(Command::CurveTo(a, b, p));
    }
    fn close(&mut self) { self.cmds.push(Command::Close); }
}

/// Pintor minimo: pilha de transformacoes, de mascaras de recorte (ja
/// intersectadas) e de camadas RGBA pre-multiplicadas.
struct Painter<'a> {
    font: &'a FontRef<'a>,
    palette: Vec<[f32; 4]>,
    fg: [f32; 4],
    w: usize,
    h: usize,
    xf: Vec<Transform>,
    clips: Vec<Vec<f32>>,
    layers: Vec<(Vec<[f32; 4]>, CompositeMode)>,
}

impl Painter<'_> {
    fn cur(&self) -> Transform { *self.xf.last().unwrap() }

    fn push_mask(&mut self, cmds: &[Command]) {
        let mut buf = vec![0u8; self.w * self.h];
        if !cmds.is_empty() {
            Mask::new(cmds).size(self.w as u32, self.h as u32).style(Fill::NonZero).render_into(&mut buf, None);
        }
        let mut m: Vec<f32> = buf.iter().map(|v| *v as f32 / 255.0).collect();
        if let Some(prev) = self.clips.last() { for (a, b) in m.iter_mut().zip(prev) { *a *= b; } }
        self.clips.push(m);
    }

    fn coverage(&self, i: usize) -> f32 { self.clips.last().map_or(1.0, |c| c[i]) }

    /// Cor da paleta (rgba direto, sem pre-multiplicar).
    fn color(&self, index: u16, alpha: f32) -> [f32; 4] {
        let mut c = if index == 0xFFFF { self.fg } else { self.palette.get(index as usize).copied().unwrap_or(self.fg) };
        c[3] *= alpha;
        c
    }

    fn stop_color(&self, stops: &[ColorStop], t: f32) -> [f32; 4] {
        let (first, last) = (stops[0], stops[stops.len() - 1]);
        if t <= first.offset { return self.color(first.palette_index, first.alpha); }
        if t >= last.offset { return self.color(last.palette_index, last.alpha); }
        for pair in stops.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if t <= b.offset {
                let span = b.offset - a.offset;
                let k = if span > 0.0 { (t - a.offset) / span } else { 1.0 };
                let (ca, cb) = (self.color(a.palette_index, a.alpha), self.color(b.palette_index, b.alpha));
                return std::array::from_fn(|i| ca[i] + (cb[i] - ca[i]) * k);
            }
        }
        self.color(last.palette_index, last.alpha)
    }
}

fn extend(t: f32, e: Extend) -> f32 {
    match e {
        Extend::Repeat => t - t.floor(),
        Extend::Reflect => { let m = t.rem_euclid(2.0); if m > 1.0 { 2.0 - m } else { m } }
        _ => t.clamp(0.0, 1.0),
    }
}

/// Gradiente radial de dois circulos (como no canvas): maior t com raio >= 0.
fn radial_t(p: (f32, f32), c0: (f32, f32), r0: f32, c1: (f32, f32), r1: f32) -> Option<f32> {
    let (cdx, cdy) = (c1.0 - c0.0, c1.1 - c0.1);
    let (pdx, pdy) = (p.0 - c0.0, p.1 - c0.1);
    let dr = r1 - r0;
    let a = cdx * cdx + cdy * cdy - dr * dr;
    let b = pdx * cdx + pdy * cdy + r0 * dr;
    let c = pdx * pdx + pdy * pdy - r0 * r0;
    let ok = |t: f32| r0 + t * dr >= 0.0;
    if a.abs() < 1e-6 {
        if b.abs() < 1e-6 { return None; }
        let t = c / (2.0 * b);
        return ok(t).then_some(t);
    }
    let disc = b * b - a * c;
    if disc < 0.0 { return None; }
    let sq = disc.sqrt();
    let (t1, t2) = ((b + sq) / a, (b - sq) / a);
    let (hi, lo) = if t1 > t2 { (t1, t2) } else { (t2, t1) };
    if ok(hi) { Some(hi) } else if ok(lo) { Some(lo) } else { None }
}

/// Composicao de pixels pre-multiplicados (src sobre dst).
fn composite(s: [f32; 4], d: [f32; 4], mode: CompositeMode) -> [f32; 4] {
    use CompositeMode as M;
    let (sa, da) = (s[3], d[3]);
    let pd = |fs: f32, fd: f32| -> [f32; 4] { std::array::from_fn(|i| s[i] * fs + d[i] * fd) };
    let blend = |f: fn(f32, f32) -> f32| -> [f32; 4] {
        let mut o = [0.0; 4];
        for i in 0..3 {
            let (cs, cb) = (if sa > 0.0 { s[i] / sa } else { 0.0 }, if da > 0.0 { d[i] / da } else { 0.0 });
            o[i] = s[i] * (1.0 - da) + d[i] * (1.0 - sa) + sa * da * f(cb, cs).clamp(0.0, 1.0);
        }
        o[3] = sa + da - sa * da;
        o
    };
    match mode {
        M::Clear => [0.0; 4],
        M::Src => s,
        M::Dest => d,
        M::DestOver => pd(1.0 - da, 1.0),
        M::SrcIn => pd(da, 0.0),
        M::DestIn => pd(0.0, sa),
        M::SrcOut => pd(1.0 - da, 0.0),
        M::DestOut => pd(0.0, 1.0 - sa),
        M::SrcAtop => pd(da, 1.0 - sa),
        M::DestAtop => pd(1.0 - da, sa),
        M::Xor => pd(1.0 - da, 1.0 - sa),
        M::Plus => std::array::from_fn(|i| (s[i] + d[i]).min(1.0)),
        M::Screen => blend(|b, s| b + s - b * s),
        M::Multiply => blend(|b, s| b * s),
        M::Darken => blend(|b, s| b.min(s)),
        M::Lighten => blend(|b, s| b.max(s)),
        M::Difference => blend(|b, s| (b - s).abs()),
        M::Exclusion => blend(|b, s| b + s - 2.0 * b * s),
        M::Overlay => blend(|b, s| if b <= 0.5 { 2.0 * b * s } else { 1.0 - 2.0 * (1.0 - b) * (1.0 - s) }),
        M::HardLight => blend(|b, s| if s <= 0.5 { 2.0 * b * s } else { 1.0 - 2.0 * (1.0 - b) * (1.0 - s) }),
        M::ColorDodge => blend(|b, s| if b <= 0.0 { 0.0 } else if s >= 1.0 { 1.0 } else { (b / (1.0 - s)).min(1.0) }),
        M::ColorBurn => blend(|b, s| if b >= 1.0 { 1.0 } else if s <= 0.0 { 0.0 } else { 1.0 - ((1.0 - b) / s).min(1.0) }),
        M::SoftLight => blend(|b, s| if s <= 0.5 { b - (1.0 - 2.0 * s) * b * (1.0 - b) } else {
            let d = if b <= 0.25 { ((16.0 * b - 12.0) * b + 4.0) * b } else { b.sqrt() };
            b + (2.0 * s - 1.0) * (d - b)
        }),
        // Modos HSL e desconhecidos: aproxima por src-over.
        _ => pd(1.0, 1.0 - sa),
    }
}

impl ColorPainter for Painter<'_> {
    fn push_transform(&mut self, t: Transform) { let n = mul(&self.cur(), &t); self.xf.push(n); }

    fn pop_transform(&mut self) { if self.xf.len() > 1 { self.xf.pop(); } }

    fn push_clip_glyph(&mut self, gid: GlyphId) {
        let mut pen = Pen { t: self.cur(), cmds: Vec::new() };
        if let Some(g) = self.font.outline_glyphs().get(gid) {
            let _ = g.draw(DrawSettings::unhinted(Size::unscaled(), LocationRef::default()), &mut pen);
        }
        self.push_mask(&pen.cmds);
    }

    fn push_clip_box(&mut self, b: BoundingBox<f32>) {
        let t = self.cur();
        let pts = [(b.x_min, b.y_min), (b.x_max, b.y_min), (b.x_max, b.y_max), (b.x_min, b.y_max)]
            .map(|(x, y)| { let (x, y) = apply(&t, x, y); Vector::new(x, y) });
        let cmds = [Command::MoveTo(pts[0]), Command::LineTo(pts[1]), Command::LineTo(pts[2]), Command::LineTo(pts[3]), Command::Close];
        self.push_mask(&cmds);
    }

    fn pop_clip(&mut self) { self.clips.pop(); }

    fn fill(&mut self, brush: Brush<'_>) {
        let Some(inv) = invert(&self.cur()) else { return };
        let mut layer = std::mem::take(&mut self.layers.last_mut().unwrap().0);
        let solid = match &brush {
            Brush::Solid { palette_index, alpha } => Some(self.color(*palette_index, *alpha)),
            Brush::LinearGradient { color_stops, .. } | Brush::RadialGradient { color_stops, .. } | Brush::SweepGradient { color_stops, .. } => {
                if color_stops.is_empty() { self.layers.last_mut().unwrap().0 = layer; return; }
                (color_stops.len() == 1).then(|| self.color(color_stops[0].palette_index, color_stops[0].alpha))
            }
        };
        let at = |x: f32, y: f32| -> Option<[f32; 4]> {
            if let Some(c) = solid { return Some(c); }
            let (fx, fy) = apply(&inv, x, y);
            match &brush {
                Brush::LinearGradient { p0, p1, color_stops, extend: e } => {
                    let (dx, dy) = (p1.x - p0.x, p1.y - p0.y);
                    let len = dx * dx + dy * dy;
                    let t = if len > 0.0 { ((fx - p0.x) * dx + (fy - p0.y) * dy) / len } else { 0.0 };
                    Some(self.stop_color(color_stops, extend(t, *e)))
                }
                Brush::RadialGradient { c0, r0, c1, r1, color_stops, extend: e } => {
                    let t = radial_t((fx, fy), (c0.x, c0.y), *r0, (c1.x, c1.y), *r1)?;
                    Some(self.stop_color(color_stops, extend(t, *e)))
                }
                Brush::SweepGradient { c0, start_angle, end_angle, color_stops, extend: e } => {
                    // Angulos ja vem no sentido horario (y para cima da fonte).
                    let a = (-(fy - c0.y)).atan2(fx - c0.x).to_degrees().rem_euclid(360.0);
                    let span = end_angle - start_angle;
                    let t = if span.abs() > 1e-6 { (a - start_angle) / span } else if a < *start_angle { 0.0 } else { 1.0 };
                    Some(self.stop_color(color_stops, extend(t, *e)))
                }
                Brush::Solid { .. } => None,
            }
        };
        let (w, h) = (self.w, self.h);
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let cov = self.coverage(i);
                if cov <= 0.0 { continue; }
                let Some(c) = at(x as f32 + 0.5, y as f32 + 0.5) else { continue };
                let a = c[3] * cov;
                let d = &mut layer[i];
                for k in 0..3 { d[k] = c[k] * a + d[k] * (1.0 - a); }
                d[3] = a + d[3] * (1.0 - a);
            }
        }
        self.layers.last_mut().unwrap().0 = layer;
    }

    fn push_layer(&mut self, mode: CompositeMode) { self.layers.push((vec![[0.0; 4]; self.w * self.h], mode)); }

    fn pop_layer_with_mode(&mut self, mode: CompositeMode) {
        if self.layers.len() < 2 { return; }
        let (src, _) = self.layers.pop().unwrap();
        let mut dst = std::mem::take(&mut self.layers.last_mut().unwrap().0);
        for (i, (d, s)) in dst.iter_mut().zip(&src).enumerate() {
            let cov = self.coverage(i);
            if cov <= 0.0 { continue; }
            let o = composite(*s, *d, mode);
            for k in 0..4 { d[k] += (o[k] - d[k]) * cov; }
        }
        self.layers.last_mut().unwrap().0 = dst;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composicao_basica() {
        let s = [0.5, 0.0, 0.0, 0.5];
        let d = [0.0, 0.0, 1.0, 1.0];
        assert_eq!(composite(s, d, CompositeMode::SrcOver), [0.5, 0.0, 0.5, 1.0]);
        assert_eq!(composite(s, d, CompositeMode::DestIn), [0.0, 0.0, 0.5, 0.5]);
        assert_eq!(composite(s, d, CompositeMode::Clear), [0.0; 4]);
    }

    #[test]
    fn gradiente_radial_concentrico() {
        // Circulo de raio 0 a 10 no mesmo centro: t = distancia / 10.
        let t = radial_t((5.0, 0.0), (0.0, 0.0), 0.0, (0.0, 0.0), 10.0).unwrap();
        assert!((t - 0.5).abs() < 1e-4);
    }

    #[test]
    fn transformacao_inversa() {
        let t = Transform { xx: 2.0, yx: 0.0, xy: 0.0, yy: -2.0, dx: 3.0, dy: 7.0 };
        let i = invert(&t).unwrap();
        let (x, y) = apply(&t, 1.5, -4.0);
        let (bx, by) = apply(&i, x, y);
        assert!((bx - 1.5).abs() < 1e-5 && (by + 4.0).abs() < 1e-5);
        assert_eq!(extend(1.25, Extend::Repeat), 0.25);
        assert_eq!(extend(1.25, Extend::Reflect), 0.75);
    }

    /// Com o Noto Color Emoji COLRv1 instalado (Fedora), o emoji sai com cor.
    #[test]
    fn noto_colrv1_rasteriza() {
        // Fora do Fedora (ou no container de build), copie a fonte para target/.
        let paths = ["/usr/share/fonts/google-noto-color-emoji-fonts/Noto-COLRv1.ttf", "target/Noto-COLRv1.ttf"];
        let Some(data) = paths.iter().find_map(|p| std::fs::read(p).ok()) else { return };
        assert!(has_v1(&data, 0));
        let font = FontRef::from_index(&data, 0).unwrap();
        let gid = font.charmap().map('\u{1F525}').unwrap().to_u32() as u16; // 🔥
        assert!(is_v1_only(&data, 0, gid));
        let (l, t, w, h) = bounds(&data, 0, gid, 32.0).unwrap();
        let img = rasterize(&data, 0, gid, 32.0, l, t, w, h, [255; 4]).unwrap();
        let opaque = img.chunks(4).filter(|p| p[3] > 200).count();
        let colored = img.chunks(4).filter(|p| p[3] > 200 && p[0] > 180 && p[2] < 120).count();
        assert!(opaque > (w * h) as usize / 5, "pouco pintado: {opaque} de {}", w * h);
        assert!(colored > 20, "fogo sem laranja/vermelho: {colored}");
    }
}
