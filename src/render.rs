//! Render na GPU: retangulos (fundo de celula, cursor, bordas, selecao) por um
//! pipeline instanciado minimo, e texto pelo glyphon.
//!
//! Camadas: 0 = panes, 1 = pane flutuante, 2 = overlay (paleta, avisos). Cada
//! camada pinta os retangulos e depois o texto, para o que vem depois ficar
//! por cima.
//!
//! Opcionais: imagem de fundo (atras de tudo) e shader de pos-processamento
//! (a cena vai para uma textura e um fragment shader do usuario a desenha).
//!
//! Cache de texto: cada trecho (texto + cor + estilo) vira um Buffer moldado
//! uma vez e reaproveitado entre frames; o que nao aparece num frame sai do
//! cache. Moldar texto e o custo caro; assim so o que mudou e moldado.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, Style, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};
use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::config::Rgb;

pub const LAYERS: usize = 4;

#[derive(Clone, Copy)]
pub struct Quad { pub x: f32, pub y: f32, pub w: f32, pub h: f32, pub color: [f32; 4] }

/// Retangulo com cantos arredondados, sombra/brilho (soft) e so contorno
/// (border > 0). Desenhado antes dos quads da mesma camada.
#[derive(Clone, Copy, Default)]
pub struct Shape { pub x: f32, pub y: f32, pub w: f32, pub h: f32, pub color: [f32; 4], pub radius: f32, pub soft: f32, pub border: f32 }

impl Shape {
    pub fn rect(x: f32, y: f32, w: f32, h: f32, color: [f32; 4], radius: f32) -> Shape { Shape { x, y, w, h, color, radius, ..Default::default() } }
    pub fn shadow(x: f32, y: f32, w: f32, h: f32, color: [f32; 4], radius: f32, soft: f32) -> Shape { Shape { x, y, w, h, color, radius, soft, border: 0.0 } }
    pub fn ring(x: f32, y: f32, w: f32, h: f32, color: [f32; 4], radius: f32, border: f32) -> Shape { Shape { x, y, w, h, color, radius, soft: 0.0, border } }
}

pub struct TextItem {
    pub x: f32,
    pub y: f32,
    pub text: String,
    pub color: Rgb,
    pub alpha: f32,
    pub bold: bool,
    pub italic: bool,
    /// Corta no limite direito (titulo de aba comprido, item da paleta).
    pub clip_right: Option<f32>,
    /// Fonte da interface (proporcional) em vez da do terminal.
    pub ui: bool,
    /// Multiplicador do tamanho (1.0 = tamanho do terminal).
    pub size: f32,
}

#[derive(Default)]
pub struct Layer { pub shapes: Vec<Shape>, pub quads: Vec<Quad>, pub texts: Vec<TextItem> }

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Cor para o shader: linear (superficie sRGB converte de volta) e
/// pre-multiplicada pelo alfa (opacidade da janela).
pub fn lin(c: Rgb, a: f32) -> [f32; 4] {
    let f = c.f();
    [srgb_to_linear(f[0]) * a, srgb_to_linear(f[1]) * a, srgb_to_linear(f[2]) * a, a]
}

const QUAD_SHADER: &str = r#"
struct U { size: vec2<f32>, _pad: vec2<f32> };
@group(0) @binding(0) var<uniform> u: U;
struct VOut {
    @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32>,
    @location(1) local: vec2<f32>, @location(2) half: vec2<f32>, @location(3) prm: vec4<f32>,
};
// prm = (raio, suavidade da sombra, espessura do contorno, -)
@vertex fn vs(@builtin(vertex_index) vi: u32, @location(0) rect: vec4<f32>, @location(1) color: vec4<f32>, @location(2) prm: vec4<f32>) -> VOut {
    var c = array<vec2<f32>, 6>(vec2(0., 0.), vec2(1., 0.), vec2(0., 1.), vec2(0., 1.), vec2(1., 0.), vec2(1., 1.));
    let grow = prm.y * 2.0;
    let p = rect.xy - vec2(grow) + c[vi] * (rect.zw + vec2(grow * 2.0));
    var o: VOut;
    o.pos = vec4(p.x / u.size.x * 2. - 1., 1. - p.y / u.size.y * 2., 0., 1.);
    o.color = color;
    o.half = rect.zw * 0.5;
    o.local = p - (rect.xy + o.half);
    o.prm = prm;
    return o;
}
fn sd_round(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - b + vec2(r);
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
}
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {
    if (i.prm.x == 0.0 && i.prm.y == 0.0 && i.prm.z == 0.0) { return i.color; }
    let r = min(i.prm.x, min(i.half.x, i.half.y));
    let d = sd_round(i.local, i.half, r);
    var a = 0.0;
    if (i.prm.y > 0.0) { a = 1.0 - smoothstep(-i.prm.y, i.prm.y, d); } else { a = clamp(0.5 - d, 0.0, 1.0); }
    if (i.prm.z > 0.0) { a = a * clamp(0.5 + d + i.prm.z, 0.0, 1.0); }
    return i.color * a;
}
"#;

/// Quadro inteiro amostrando uma textura. `uv` = (min.xy, max.xy) para
/// recortar a imagem de fundo em modo "cover"; `p` = (alfa, tempo, w, h).
const TEX_SHADER: &str = r#"
struct U { uv: vec4<f32>, p: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var smp: sampler;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@builtin(vertex_index) vi: u32) -> VOut {
    var c = array<vec2<f32>, 6>(vec2(0., 0.), vec2(1., 0.), vec2(0., 1.), vec2(0., 1.), vec2(1., 0.), vec2(1., 1.));
    var o: VOut;
    o.pos = vec4(c[vi].x * 2. - 1., 1. - c[vi].y * 2., 0., 1.);
    o.uv = mix(u.uv.xy, u.uv.zw, c[vi]);
    return o;
}
"#;

const TEX_FS_IMAGE: &str = r#"
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {
    let c = textureSample(tex, smp, i.uv);
    return vec4(c.rgb * u.p.x, u.p.x);
}
"#;

/// O shader do usuario define `fn effect(uv: vec2<f32>, time: f32, size: vec2<f32>) -> vec4<f32>`
/// e pode chamar `scene(uv)` para ler a tela ja desenhada.
const TEX_FS_POST: &str = r#"
fn scene(uv: vec2<f32>) -> vec4<f32> { return textureSample(tex, smp, uv); }
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> { return effect(i.uv, u.p.y, u.p.zw); }
"#;

struct Quads { pipeline: wgpu::RenderPipeline, uniform: wgpu::Buffer, bind: wgpu::BindGroup }

impl Quads {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Quads {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("quads"), source: wgpu::ShaderSource::Wgsl(QUAD_SHADER.into()),
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: None, size: 16, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0, visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None, layout: &bgl, entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None, bind_group_layouts: &[Some(&bgl)], immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("quads"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module, entry_point: Some("vs"), compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 48, step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module, entry_point: Some("fs"), compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format, blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING), write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Quads { pipeline, uniform, bind }
    }
}

/// Pipeline de "quadro inteiro com textura": imagem de fundo e pos-processamento.
struct TexPass { pipeline: wgpu::RenderPipeline, bgl: wgpu::BindGroupLayout, uniform: wgpu::Buffer, sampler: wgpu::Sampler }

impl TexPass {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat, fs: &str) -> Result<TexPass, String> {
        let src = format!("{TEX_SHADER}\n{fs}");
        // Shader do usuario com erro nao pode derrubar a janela: valida antes.
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("tex"), source: wgpu::ShaderSource::Wgsl(src.into()) });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: None, size: 32, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0, visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1, visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2, visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("tex"),
            layout: Some(&layout),
            vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            fragment: Some(wgpu::FragmentState {
                module: &module, entry_point: Some("fs"), compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING), write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        if let Some(e) = pollster::block_on(scope.pop()) { return Err(e.to_string()); }
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default()
        });
        Ok(TexPass { pipeline, bgl, uniform, sampler })
    }
    fn bind(&self, device: &wgpu::Device, view: &wgpu::TextureView) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None, layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        })
    }
}

struct BgImage { pass: TexPass, bind: wgpu::BindGroup, w: f32, h: f32, alpha: f32 }

struct Post { pass: TexPass, target: Option<(u32, u32, wgpu::Texture, wgpu::BindGroup)>, start: Instant }

pub struct Renderer {
    pub window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
    font_system: FontSystem,
    swash: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    glyph_cache: Cache,
    text: Vec<TextRenderer>,
    quads: Quads,
    bg: Option<BgImage>,
    post: Option<Post>,
    cache: HashMap<u64, Buffer>,
    widths: HashMap<u64, f32>,
    pub family: String,
    pub ui_family: String,
    /// Peso da fonte do terminal (400 = normal).
    pub weight: u16,
    features: Vec<(String, u32)>,
    pub font_px: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    line_height: f32,
    pub transparent: bool,
    /// GPU perdida (driver reiniciou, TDR): a GUI recria o Renderer.
    pub lost: Arc<AtomicBool>,
}

impl Renderer {
    pub fn new(window: Arc<Window>, family: &str, font_dirs: &[String], font_pt: f32, line_height: f32, want_alpha: bool, backend: &str) -> Renderer {
        let size = window.inner_size();
        // Um backend so: carregar todos custa RAM a toa. WGPU_BACKEND no env sobrescreve.
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = match backend.to_lowercase().as_str() {
            "vulkan" => wgpu::Backends::VULKAN,
            "gl" | "opengl" => wgpu::Backends::GL,
            _ if cfg!(windows) => wgpu::Backends::DX12,
            _ => wgpu::Backends::VULKAN,
        };
        // Swapchain por DirectComposition: so ela tem alfa (janela translucida,
        // Acrylic/Mica aparecendo atras). Fica so quando a config pede.
        if want_alpha { desc.backend_options.dx12.presentation_system = wgpu::Dx12SwapchainKind::DxgiFromVisual; }
        let instance = wgpu::Instance::new(desc.with_env());
        let surface = instance.create_surface(window.clone()).expect("surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            ..Default::default()
        })).expect("nenhuma GPU compativel");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        })).expect("device");
        let lost = Arc::new(AtomicBool::new(false));
        {
            let lost = lost.clone();
            device.set_device_lost_callback(move |_, msg| {
                crate::proto::log(&format!("GPU perdida: {msg}"));
                lost.store(true, Ordering::SeqCst);
            });
        }
        let mut config = surface.get_default_config(&adapter, size.width.max(1), size.height.max(1)).expect("config da superficie");
        let caps = surface.get_capabilities(&adapter);
        let mut transparent = false;
        if want_alpha {
            for m in [wgpu::CompositeAlphaMode::PreMultiplied, wgpu::CompositeAlphaMode::PostMultiplied] {
                if caps.alpha_modes.contains(&m) { config.alpha_mode = m; transparent = true; break; }
            }
        }
        config.present_mode = wgpu::PresentMode::AutoVsync;
        config.usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
        surface.configure(&device, &config);

        let mut font_system = FontSystem::new();
        // Nerd Font embutida: icones funcionam sem instalar fonte nenhuma.
        font_system.db_mut().load_font_data(NERD_FONT.to_vec());
        for d in font_dirs { font_system.db_mut().load_fonts_dir(d); }
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, config.format);
        let text = (0..LAYERS).map(|_| TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None)).collect();
        let quads = Quads::new(&device, config.format);
        let scale = window.scale_factor() as f32;
        let mut r = Renderer {
            window, surface, device, queue, config, swash: SwashCache::new(), viewport, atlas, glyph_cache: cache, text, quads,
            bg: None, post: None, cache: HashMap::new(), widths: HashMap::new(), family: family.into(), ui_family: String::new(),
            weight: 400, features: Vec::new(), font_px: 0.0, cell_w: 8.0, cell_h: 16.0,
            line_height, transparent, font_system, lost,
        };
        r.set_font(family, font_pt * 96.0 / 72.0 * scale, line_height);
        r
    }

    /// Troca fonte/tamanho e remede a celula (largura do "M" moldado).
    pub fn set_font(&mut self, family: &str, px: f32, line_height: f32) {
        // Primeira familia que existir; senao a monoespacada padrao do sistema.
        let db = self.font_system.db();
        let has = |name: &str| db.faces().any(|f| f.families.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)));
        self.family = [family, "JetBrainsMono Nerd Font Mono", "JetBrains Mono", "Cascadia Mono", "Consolas"]
            .into_iter().find(|f| has(f)).unwrap_or(family).to_string();
        if self.ui_family.is_empty() || !has(&self.ui_family) {
            self.ui_family = ["Segoe UI Variable Text", "Segoe UI", "Inter", "Noto Sans", "DejaVu Sans"]
                .into_iter().find(|f| has(f)).unwrap_or(&self.family).to_string();
        }
        self.font_px = px.clamp(6.0, 72.0);
        self.line_height = line_height;
        self.cell_h = (self.font_px * line_height).round();
        let mut b = Buffer::new(&mut self.font_system, Metrics::new(self.font_px, self.cell_h));
        b.set_text("MMMMMMMMMM", &Attrs::new().family(Family::Name(&self.family)), Shaping::Advanced, None);
        b.shape_until_scroll(&mut self.font_system, false);
        self.cell_w = b.layout_runs().next().map(|r| r.line_w / 10.0).filter(|w| *w > 1.0).unwrap_or(self.font_px * 0.6);
        self.cache.clear();
        self.widths.clear();
    }

    /// Fonte da interface, peso e recursos OpenType ("-calt", "ss01").
    pub fn set_style(&mut self, ui_font: &str, weight: u16, features: &[String]) {
        let db = self.font_system.db();
        if db.faces().any(|f| f.families.iter().any(|(n, _)| n.eq_ignore_ascii_case(ui_font))) { self.ui_family = ui_font.to_string(); }
        self.weight = weight.clamp(100, 900);
        self.features = features.iter().filter_map(|f| {
            let (name, v) = match f.strip_prefix('-') { Some(n) => (n, 0), None => (f.trim_start_matches('+'), 1) };
            (name.len() == 4).then(|| (name.to_string(), v))
        }).collect();
        self.cache.clear();
        self.widths.clear();
    }

    fn shape(&mut self, t: &TextItem) -> Buffer {
        let size = if t.size > 0.0 { t.size } else { 1.0 };
        let fam = if t.ui { self.ui_family.clone() } else { self.family.clone() };
        let mut attrs = Attrs::new().family(Family::Name(&fam));
        let w = if t.bold { (self.weight + 300).min(900) } else { self.weight };
        if !t.ui { attrs = attrs.weight(Weight(w)); } else if t.bold { attrs = attrs.weight(Weight(600)); }
        if t.italic { attrs = attrs.style(Style::Italic); }
        if !t.ui && !self.features.is_empty() {
            let mut ff = glyphon::cosmic_text::FontFeatures::new();
            for (n, v) in &self.features {
                let b = n.as_bytes();
                ff.set(glyphon::cosmic_text::FeatureTag::new(&[b[0], b[1], b[2], b[3]]), *v);
            }
            attrs = attrs.font_features(ff);
        }
        let mut b = Buffer::new(&mut self.font_system, Metrics::new(self.font_px * size, self.cell_h * size));
        b.set_size(None, None);
        b.set_text(&text_glyphs(&t.text), &attrs, Shaping::Advanced, None);
        b.shape_until_scroll(&mut self.font_system, false);
        b
    }

    /// Largura do texto em pixels (texto da interface tem largura variavel).
    pub fn measure(&mut self, text: &str, ui: bool, size: f32) -> f32 {
        let size = if size > 0.0 { size } else { 1.0 };
        if !ui { return text.chars().count() as f32 * self.cell_w * size; }
        let item = TextItem { x: 0.0, y: 0.0, text: text.into(), color: Rgb(0, 0, 0), alpha: 1.0, bold: false, italic: false, clip_right: None, ui, size };
        let k = Self::key(&item);
        if let Some(w) = self.widths.get(&k) { return *w; }
        let b = self.shape(&item);
        let w = b.layout_runs().map(|r| r.line_w).fold(0.0, f32::max);
        if self.widths.len() > 4000 { self.widths.clear(); }
        self.widths.insert(k, w);
        w
    }

    /// Familias monoespacadas instaladas (para a tela de configuracao).
    pub fn mono_families(&self) -> Vec<String> {
        let mut v: Vec<String> = self.font_system.db().faces().filter(|f| f.monospaced)
            .filter_map(|f| f.families.first().map(|x| x.0.clone())).collect();
        v.sort();
        v.dedup();
        v
    }

    /// Imagem de fundo (png/jpg), recortada em "cover", com alfa.
    pub fn set_background(&mut self, path: Option<&str>, alpha: f32) -> Result<(), String> {
        self.bg = None;
        let Some(path) = path else { return Ok(()) };
        let img = image::open(path).map_err(|e| format!("{path}: {e}"))?.to_rgba8();
        let (w, h) = img.dimensions();
        let size = wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 };
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("fundo"), size, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb, usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &img, wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(h) }, size,
        );
        let pass = TexPass::new(&self.device, self.config.format, TEX_FS_IMAGE)?;
        let view = tex.create_view(&Default::default());
        let bind = pass.bind(&self.device, &view);
        self.bg = Some(BgImage { pass, bind, w: w as f32, h: h as f32, alpha: alpha.clamp(0.0, 1.0) });
        Ok(())
    }

    /// Shader de pos-processamento do usuario (WGSL com `fn effect(...)`).
    pub fn set_shader(&mut self, path: Option<&str>) -> Result<(), String> {
        self.post = None;
        let Some(path) = path else { return Ok(()) };
        let user = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        let pass = TexPass::new(&self.device, self.config.format, &format!("{user}\n{TEX_FS_POST}"))?;
        self.post = Some(Post { pass, target: None, start: Instant::now() });
        Ok(())
    }

    pub fn animated(&self) -> bool { self.post.is_some() }

    /// Janela minimizada: solta o que se refaz no proximo quadro (textos
    /// moldados, atlas de glifos, alvo do shader) e devolve as paginas ao
    /// Windows. A troca de cadeia ja encolhe para 1x1 no resize(0, 0).
    pub fn hibernate(&mut self) {
        self.cache.clear();
        self.cache.shrink_to_fit();
        self.swash = SwashCache::new();
        self.atlas = TextAtlas::new(&self.device, &self.queue, &self.glyph_cache, self.config.format);
        self.text = (0..LAYERS).map(|_| TextRenderer::new(&mut self.atlas, &self.device, wgpu::MultisampleState::default(), None)).collect();
        if let Some(p) = self.post.as_mut() { p.target = None; }
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
            SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
        }
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        self.config.width = w.max(1);
        self.config.height = h.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    pub fn width(&self) -> f32 { self.config.width as f32 }
    pub fn height(&self) -> f32 { self.config.height as f32 }

    fn key(item: &TextItem) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        item.text.hash(&mut h);
        (item.color.0, item.color.1, item.color.2, item.bold, item.italic, (item.alpha * 255.0) as u8, item.ui, (item.size * 100.0) as u32).hash(&mut h);
        h.finish()
    }

    pub fn draw(&mut self, clear: [f32; 4], layers: &[Layer]) {
        let (w, h) = (self.config.width, self.config.height);
        self.viewport.update(&self.queue, Resolution { width: w, height: h });
        self.queue.write_buffer(&self.quads.uniform, 0, bytemuck_f32(&[w as f32, h as f32, 0.0, 0.0]));

        // Molda o que nao estiver no cache.
        let mut used = HashSet::new();
        for layer in layers {
            for t in &layer.texts {
                let k = Self::key(t);
                used.insert(k);
                if !self.cache.contains_key(&k) {
                    let b = self.shape(t);
                    self.cache.insert(k, b);
                }
            }
        }
        self.cache.retain(|k, _| used.contains(k));

        let mut quad_bufs = Vec::new();
        for (li, layer) in layers.iter().enumerate().take(LAYERS) {
            let areas: Vec<TextArea> = layer.texts.iter().map(|t| {
                let c = t.color;
                TextArea {
                    buffer: &self.cache[&Self::key(t)],
                    left: t.x,
                    top: t.y,
                    scale: 1.0,
                    bounds: TextBounds { left: 0, top: 0, right: t.clip_right.map(|r| r as i32).unwrap_or(w as i32), bottom: h as i32 },
                    default_color: Color::rgba(c.0, c.1, c.2, (t.alpha * 255.0) as u8),
                    custom_glyphs: &[],
                }
            }).collect();
            let _ = self.text[li].prepare(&self.device, &self.queue, &mut self.font_system, &mut self.atlas,
                &self.viewport, areas, &mut self.swash);
            // Formas primeiro (fundos de cartao, sombras), depois os quads da grade.
            let data: Vec<f32> = layer.shapes.iter().flat_map(|q| [q.x, q.y, q.w, q.h, q.color[0], q.color[1], q.color[2], q.color[3], q.radius, q.soft, q.border, 0.0])
                .chain(layer.quads.iter().flat_map(|q| [q.x, q.y, q.w, q.h, q.color[0], q.color[1], q.color[2], q.color[3], 0.0, 0.0, 0.0, 0.0])).collect();
            quad_bufs.push((!data.is_empty()).then(|| self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None, contents: bytemuck_f32(&data), usage: wgpu::BufferUsages::VERTEX,
            })));
        }
        if let Some(bg) = &self.bg {
            // "cover": recorta a imagem para a proporcao da janela.
            let (ww, wh) = (w as f32, h as f32);
            let (sx, sy) = if bg.w / bg.h > ww / wh { ((ww / wh) * bg.h / bg.w, 1.0) } else { (1.0, (wh / ww) * bg.w / bg.h) };
            let u = [(1.0 - sx) / 2.0, (1.0 - sy) / 2.0, (1.0 + sx) / 2.0, (1.0 + sy) / 2.0, bg.alpha, 0.0, ww, wh];
            self.queue.write_buffer(&bg.pass.uniform, 0, bytemuck_f32(&u));
        }

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            _ => return,
        };
        let screen = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        // Com pos-processamento a cena vai para uma textura intermediaria.
        if let Some(post) = self.post.as_mut() {
            if post.target.as_ref().is_none_or(|t| t.0 != w || t.1 != h) {
                let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("cena"), size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                    mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: self.config.format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[],
                });
                let bind = post.pass.bind(&self.device, &tex.create_view(&Default::default()));
                post.target = Some((w, h, tex, bind));
            }
            let t = post.start.elapsed().as_secs_f32();
            self.queue.write_buffer(&post.pass.uniform, 0, bytemuck_f32(&[0.0, 0.0, 1.0, 1.0, 1.0, t, w as f32, h as f32]));
        }
        let scene_view = self.post.as_ref().and_then(|p| p.target.as_ref()).map(|t| t.2.create_view(&Default::default()));
        let target = scene_view.as_ref().unwrap_or(&screen);
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target, resolve_target: None, depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: clear[0] as f64, g: clear[1] as f64, b: clear[2] as f64, a: clear[3] as f64 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
            });
            if let Some(bg) = &self.bg {
                pass.set_pipeline(&bg.pass.pipeline);
                pass.set_bind_group(0, &bg.bind, &[]);
                pass.draw(0..6, 0..1);
            }
            for (li, layer) in layers.iter().enumerate().take(LAYERS) {
                if let Some(buf) = &quad_bufs[li] {
                    pass.set_pipeline(&self.quads.pipeline);
                    pass.set_bind_group(0, &self.quads.bind, &[]);
                    pass.set_vertex_buffer(0, buf.slice(..));
                    pass.draw(0..6, 0..(layer.shapes.len() + layer.quads.len()) as u32);
                }
                let _ = self.text[li].render(&self.atlas, &self.viewport, &mut pass);
            }
        }
        if let Some(post) = &self.post {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pos"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &screen, resolve_target: None, depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
            });
            pass.set_pipeline(&post.pass.pipeline);
            pass.set_bind_group(0, &post.target.as_ref().unwrap().3, &[]);
            pass.draw(0..6, 0..1);
        }
        self.queue.submit([enc.finish()]);
        self.queue.present(frame);
        self.atlas.trim();
    }
}

/// Simbolos que no Linux so existem em fonte de emoji colorido (Noto Color
/// Emoji), que nao sai na tela: ficavam em branco, como o check das opcoes
/// marcadas do Claude Code. Troca pelo equivalente da fonte embutida, de
/// mesma largura.
fn text_glyphs(s: &str) -> std::borrow::Cow<'_, str> {
    const MAP: [(char, char); 3] = [('\u{2714}', '\u{2713}'), ('\u{2716}', '\u{2715}'), ('\u{2718}', '\u{2717}')];
    if !s.chars().any(|c| MAP.iter().any(|(f, _)| *f == c)) { return std::borrow::Cow::Borrowed(s); }
    s.chars().map(|c| MAP.iter().find(|(f, _)| *f == c).map_or(c, |(_, t)| *t)).collect::<String>().into()
}

const NERD_FONT: &[u8] = include_bytes!("../fonts/JetBrainsMonoNerdFontMono-Regular.ttf");

fn bytemuck_f32(v: &[f32]) -> &[u8] {
    // f32 -> bytes sem crate extra: layout de f32 e fixo e alinhado.
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simbolos_de_emoji_viram_os_da_fonte() {
        assert_eq!(text_glyphs("[✔] ok ✖ ✘"), "[✓] ok ✕ ✗");
        assert!(matches!(text_glyphs("[ ] sem troca ✓"), std::borrow::Cow::Borrowed(_)));
    }
}
