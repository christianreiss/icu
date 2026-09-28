//! Offscreen hardware rendering. Native presentation stays click-through;
//! two asynchronous readback slots bound memory and never wait on the GPU.
use super::*;
use eframe::wgpu::{self, util::DeviceExt};
use std::sync::mpsc as channel;

pub(super) const HALO: u32 = 24;
pub(super) const GPU_TICK: Duration = Duration::from_nanos(1_000_000_000 / 30);

pub(super) struct RenderScene<'a> {
    pub geometry: Geometry,
    pub scale: f32,
    pub tiles: &'a [Tile],
    pub intensities: &'a [f32],
    pub labels: &'a mut Option<Labels>,
    pub visible_frames: &'a [(Session, u64)],
}

struct Pending {
    buffer: wgpu::Buffer,
    done: channel::Receiver<Result<(), wgpu::BufferAsyncError>>,
    at: Instant,
    size: (u32, u32),
    stride: u32,
    frames: Vec<(Session, u64)>,
}
struct CachedVideo {
    pixels: Arc<Vec<u8>>,
    texture: wgpu::Texture,
}
struct CachedLabel {
    key: (String, u32, u32, u32, bool, bool),
    texture: wgpu::Texture,
}
pub(super) struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    failed: Arc<AtomicBool>,
    pending: std::collections::VecDeque<Pending>,
    labels: Vec<CachedLabel>,
    start: Instant,
    videos: Vec<Option<CachedVideo>>,
    target: Option<wgpu::Texture>,
    buffers: Vec<wgpu::Buffer>,
}

impl Renderer {
    pub(super) fn new() -> Result<Self, String> {
        Self::with_backends(wgpu::Backends::VULKAN | wgpu::Backends::DX12)
    }

    fn with_backends(backends: wgpu::Backends) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        runtime.block_on(async {
            let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
            desc.backends = backends;
            #[cfg(windows)]
            {
                desc.backend_options.dx12.shader_compiler = wgpu::Dx12Compiler::Fxc;
            }
            let instance = wgpu::Instance::new(desc);
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions::default())
                .await
                .map_err(|e| e.to_string())?;
            if !matches!(
                adapter.get_info().device_type,
                wgpu::DeviceType::IntegratedGpu | wgpu::DeviceType::DiscreteGpu
            ) {
                return Err("no compatible hardware GPU".into());
            }
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor::default())
                .await
                .map_err(|e| e.to_string())?;
            let failed = Arc::new(AtomicBool::new(false));
            let flag = failed.clone();
            device.set_device_lost_callback(move |_, _| {
                flag.store(true, Ordering::Relaxed);
            });
            let flag = failed.clone();
            device.on_uncaptured_error(Arc::new(move |e| {
                tracing::warn!(%e, "overlay GPU error");
                flag.store(true, Ordering::Relaxed);
            }));
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("overlay nebula"),
                source: wgpu::ShaderSource::Wgsl(include_str!("nebula.wgsl").into()),
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("overlay"),
                layout: None,
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::TextureFormat::Bgra8Unorm.into())],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            });
            if failed.load(Ordering::Relaxed) {
                return Err("overlay shader initialization failed".into());
            }
            tracing::info!(adapter = %adapter.get_info().name, "overlay GPU renderer");
            Ok(Self {
                device,
                queue,
                pipeline,
                failed,
                pending: Default::default(),
                labels: Vec::new(),
                start: Instant::now(),
                videos: Vec::new(),
                target: None,
                buffers: Vec::new(),
            })
        })
    }

    pub(super) fn invalidate(&mut self) {
        self.pending.clear();
        self.labels.clear();
        self.videos.clear();
    }

    fn texture(
        &self,
        w: u32,
        h: u32,
        format: wgpu::TextureFormat,
        usage: wgpu::TextureUsages,
    ) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("overlay"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    }
    fn upload(&self, w: u32, h: u32, bytes: &[u8]) -> wgpu::Texture {
        let texture = self.texture(
            w,
            h,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        self.queue.write_texture(
            texture.as_image_copy(),
            bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: None,
            },
            texture.size(),
        );
        texture
    }

    /// Returns metadata only for the frame actually copied to the native canvas.
    pub(super) fn draw(
        &mut self,
        scene: &mut RenderScene<'_>,
        canvas: &mut [u8],
    ) -> Result<Option<Vec<(Session, u64)>>, String> {
        self.device
            .poll(wgpu::PollType::Poll)
            .map_err(|e| e.to_string())?;
        if self.failed.load(Ordering::Relaxed) {
            return Err("GPU device unavailable".into());
        }
        let mut presented = None;
        while let Some(front) = self.pending.front() {
            match front.done.try_recv() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(e.to_string()),
                Err(channel::TryRecvError::Disconnected) => {
                    return Err("GPU readback disconnected".into())
                }
                Err(channel::TryRecvError::Empty) => {
                    if front.at.elapsed() > Duration::from_secs(2) {
                        return Err("GPU readback timeout".into());
                    }
                    break;
                }
            }
            let frame = self.pending.pop_front().unwrap();
            if frame.size == (scene.geometry.width, scene.geometry.height) {
                let view = frame
                    .buffer
                    .slice(..)
                    .get_mapped_range()
                    .map_err(|e| e.to_string())?;
                for (src, dst) in view
                    .chunks_exact(frame.stride as usize)
                    .zip(canvas.chunks_exact_mut(scene.geometry.width as usize * 4))
                {
                    dst.copy_from_slice(&src[..dst.len()]);
                }
                presented = Some(frame.frames);
            }
            frame.buffer.unmap();
            self.buffers.push(frame.buffer);
        }
        if self.pending.len() >= 2 {
            return Ok(presented);
        }
        let (w, h) = (scene.geometry.width, scene.geometry.height);
        if w == 0
            || h == 0
            || w > self.device.limits().max_texture_dimension_2d
            || h > self.device.limits().max_texture_dimension_2d
        {
            return Err("overlay exceeds GPU texture limits".into());
        }
        if self
            .target
            .as_ref()
            .is_none_or(|t| t.width() != w || t.height() != h)
        {
            self.target = Some(self.texture(
                w,
                h,
                wgpu::TextureFormat::Bgra8Unorm,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            ));
            self.buffers.clear();
        }
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let mut groups = Vec::new();
        let mut boxes = Vec::new();
        let mut x = 0;
        let mut y = 0;
        let pad = scaled(HALO, scene.scale);
        for (index, tile) in scene.tiles.iter().enumerate() {
            let diameter = match tile.face {
                Face::Camera(_) => scene.geometry.camera_diameter,
                Face::Plate => scene.geometry.plate_diameter,
            };
            let reserve = if matches!(tile.face, Face::Camera(_)) {
                speak_diameter(diameter)
            } else {
                diameter
            };
            let mut geom = circle_geometry(reserve, scene.geometry.name_px);
            geom.half_w += pad;
            geom.outer_w += pad * 2;
            geom.outer_h += pad * 2;
            let tx = if scene.geometry.horizontal {
                x
            } else {
                (w - geom.outer_w) / 2
            };
            let ty = if scene.geometry.horizontal && scene.geometry.bottom {
                h - geom.outer_h
            } else {
                y
            };
            let cx = tx as f32 + geom.half_w as f32;
            let cy = ty as f32 + pad as f32 + geom.radii.r_outer;
            let r = radii(diameter as f32 * tile.scale, scene.geometry.name_px);
            let name_px = scene.geometry.name_px;
            let (shown, sweep) = scene
                .labels
                .as_mut()
                .map(|l| label_extent(l, &tile.name, name_px, r.r_label_inner, MAX_LABEL_ANGLE))
                .unwrap_or_default();
            let key = (
                tile.name.clone(),
                diameter,
                name_px,
                tile.scale.to_bits(),
                tile.dim,
                matches!(tile.face, Face::Plate),
            );
            if self.labels.get(index).is_none_or(|c| c.key != key) {
                let mut ink = vec![0; (geom.outer_w * geom.outer_h * 4) as usize];
                if let Some(labels) = scene.labels.as_mut() {
                    let color = if tile.dim {
                        LABEL_INK.map(|v| (u32::from(v) * DIM_MUL / 256) as u8)
                    } else {
                        LABEL_INK
                    };
                    draw_curved_label(
                        &mut ink,
                        geom.outer_w as usize * 4,
                        geom.outer_w,
                        geom.outer_h,
                        cx - tx as f32,
                        cy - ty as f32,
                        r.r_label_inner,
                        &shown,
                        name_px,
                        color,
                        labels,
                    );
                    if matches!(tile.face, Face::Plate) {
                        let initials = crate::ui::widgets::initials(&tile.name);
                        let label = labels.get(
                            &initials,
                            (diameter as f32 * 0.37).round() as u32,
                            diameter,
                        );
                        blend_label(
                            &mut ink,
                            geom.outer_w as usize * 4,
                            (cx - tx as f32 - label.width as f32 / 2.).round() as u32,
                            (cy - ty as f32 - label.height as f32 / 2.).round() as u32,
                            label,
                        );
                    }
                }
                for pixel in ink.as_chunks_mut::<4>().0 {
                    pixel.swap(0, 2);
                }
                let cached = CachedLabel {
                    key,
                    texture: self.upload(geom.outer_w, geom.outer_h, &ink),
                };
                if index == self.labels.len() {
                    self.labels.push(cached);
                } else {
                    self.labels[index] = cached;
                }
            }
            let (fw, fh, kind, video) = match &tile.face {
                Face::Camera(Some((fw, fh, rgba)))
                    if *fw > 0
                        && *fh > 0
                        && *fw <= self.device.limits().max_texture_dimension_2d as usize
                        && *fh <= self.device.limits().max_texture_dimension_2d as usize
                        && fw
                            .checked_mul(*fh)
                            .and_then(|v| v.checked_mul(4))
                            .is_some_and(|len| rgba.len() >= len) =>
                {
                    (*fw as f32, *fh as f32, 1., {
                        if self.videos.len() <= index {
                            self.videos.resize_with(index + 1, || None);
                        }
                        if self.videos[index].as_ref().is_none_or(|cached| {
                            !Arc::ptr_eq(&cached.pixels, rgba)
                                || cached.texture.width() != *fw as u32
                                || cached.texture.height() != *fh as u32
                        }) {
                            self.videos[index] = Some(CachedVideo {
                                pixels: rgba.clone(),
                                texture: self.upload(*fw as u32, *fh as u32, rgba),
                            });
                        }
                        self.videos[index].as_ref().unwrap().texture.clone()
                    })
                }
                _ => (
                    1.,
                    1.,
                    if matches!(tile.face, Face::Plate) {
                        2.
                    } else {
                        0.
                    },
                    self.upload(1, 1, &[0, 0, 0, 255]),
                ),
            };
            let color = scene.geometry.frame_color.map(|c| c as f32 / 255.);
            let values = [
                w as f32,
                h as f32,
                cx,
                cy,
                r.r,
                r.r_outer,
                r.r_label_inner,
                r.r_label_outer,
                self.start.elapsed().as_secs_f32() % 4096. + index as f32 * 17.0,
                if scene.geometry.speaking_frame {
                    scene.intensities.get(index).copied().unwrap_or(0.)
                } else {
                    0.0
                },
                if tile.dim { 0.5 } else { 1. },
                sweep,
                color[0],
                color[1],
                color[2],
                1.,
                tx as f32,
                ty as f32,
                geom.outer_w as f32,
                geom.outer_h as f32,
                fw,
                fh,
                kind,
                pad as f32,
            ];
            let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_ne_bytes()).collect();
            let uniform = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: None,
                    contents: &bytes,
                    usage: wgpu::BufferUsages::UNIFORM,
                });
            let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(
                            &video.create_view(&Default::default()),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(
                            &self.labels[index].texture.create_view(&Default::default()),
                        ),
                    },
                ],
            });
            groups.push(group);
            boxes.push((tx, ty, geom.outer_w, geom.outer_h));
            if scene.geometry.horizontal {
                x += geom.outer_w + GAP;
            } else {
                y += geom.outer_h + GAP;
            }
        }
        self.labels.truncate(scene.tiles.len());
        self.videos.truncate(scene.tiles.len());
        let target = self.target.as_ref().unwrap();
        {
            let view = target.create_view(&Default::default());
            let attachments = [Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })];
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &attachments,
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            for (group, (x, y, w, h)) in groups.iter().zip(boxes) {
                pass.set_scissor_rect(x, y, w, h);
                pass.set_bind_group(0, group, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        let stride = (w * 4).div_ceil(256) * 256;
        let buffer_size = u64::from(stride) * u64::from(h);
        self.buffers.retain(|b| b.size() == buffer_size);
        let buffer = self.buffers.pop().unwrap_or_else(|| {
            self.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: buffer_size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: None,
                },
            },
            target.size(),
        );
        self.queue.submit([encoder.finish()]);
        let (tx, done) = channel::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.pending.push_back(Pending {
            buffer,
            done,
            at: Instant::now(),
            size: (w, h),
            stride,
            frames: scene.visible_frames.to_vec(),
        });
        Ok(presented)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_is_time_based_and_restarts_without_a_jump() {
        let attack = fade(0., true, 0.075);
        assert!((attack - 0.5).abs() < 1e-6);
        assert_eq!(fade(attack, true, 0.075), 1.);
        let release = fade(1., false, 0.35);
        assert!((release - 0.5).abs() < 1e-6);
        assert!(fade(release, true, 0.01) > release);
        assert_eq!(fade(release, false, 0.35), 0.);
        assert_eq!(fade(0., false, 10.), 0.);
    }

    #[test]
    fn unavailable_adapter_returns_software_fallback_error() {
        assert!(Renderer::with_backends(wgpu::Backends::empty()).is_err());
    }

    fn complete(renderer: &mut Renderer, scene: &mut RenderScene<'_>, canvas: &mut [u8]) {
        let started = Instant::now();
        loop {
            if renderer.draw(scene, canvas).unwrap().is_some() {
                return;
            }
            assert!(started.elapsed() < Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    // Opt-in because CI need not have a hardware adapter. This deliberately
    // fails (does not skip) when explicitly requested without working hardware.
    #[test]
    #[ignore = "requires a hardware GPU; run with --ignored --nocapture"]
    fn hardware_render_and_device_loss() {
        let mut renderer = Renderer::new().expect("hardware GPU");
        let video_sets: Vec<Vec<Tile>> = (0..4)
            .map(|_| {
                (0..4)
                    .map(|i| Tile {
                        face: Face::Camera(Some((
                            320,
                            240,
                            Arc::new([255, 0, 0, 255].repeat(320 * 240)),
                        ))),
                        name: format!("Speaker {i}"),
                        speaking: true,
                        dim: false,
                        scale: 1.,
                    })
                    .collect()
            })
            .collect();
        for diameter in [120, 240, 360] {
            let geom = circle_geometry(speak_diameter(diameter), 12);
            let tw = geom.outer_w + HALO * 2;
            let th = geom.outer_h + HALO * 2;
            let w = tw * 4 + GAP * 3;
            let mut labels = Labels::new();
            let mut scene = RenderScene {
                geometry: Geometry {
                    width: w,
                    height: th,
                    camera_diameter: diameter,
                    plate_diameter: 72,
                    name_px: 12,
                    horizontal: true,
                    bottom: false,
                    speaking_frame: true,
                    frame_color: [46, 155, 255],
                },
                scale: 1.,
                tiles: &video_sets[0],
                intensities: &[1.; 4],
                labels: &mut labels,
                visible_frames: &[(1, 7)],
            };
            let mut canvas = vec![0; (w * th * 4) as usize];
            renderer.invalidate();
            // Exclude cold shader compilation from the steady-state budget.
            for _ in 0..5 {
                complete(&mut renderer, &mut scene, &mut canvas);
                std::thread::sleep(GPU_TICK);
            }
            let started = Instant::now();
            let mut count = 0;
            let mut times = Vec::new();
            while count < 60 {
                // Fresh decoded pictures at 15 fps, effects at 30 fps.
                scene.tiles = &video_sets[(count / 2) % video_sets.len()];
                let before = Instant::now();
                if let Some(frames) = renderer.draw(&mut scene, &mut canvas).unwrap() {
                    assert_eq!(frames, [(1, 7)]);
                    count += 1;
                }
                times.push(before.elapsed());
                assert!(started.elapsed() < Duration::from_secs(10), "GPU stalled");
                std::thread::sleep(GPU_TICK.saturating_sub(before.elapsed()));
            }
            let cx = geom.half_w + HALO;
            let cy = geom.radii.r_outer as u32 + HALO;
            let center = ((cy * w + cx) * 4) as usize;
            assert_eq!(
                &canvas[center..center + 4],
                &[0, 0, 255, 255],
                "video red must remain red in BGRA"
            );
            assert_eq!(&canvas[..4], &[0, 0, 0, 0], "transparent corners");
            assert!(
                canvas
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|p| p[3] > 0 && p[3] < 200),
                "translucent halo"
            );
            assert!(
                canvas
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|p| p[..3].iter().all(|c| *c <= p[3])),
                "premultiplied alpha"
            );
            times.sort();
            assert!(
                times[times.len() * 95 / 100] < GPU_TICK,
                "CPU draw exceeds frame budget"
            );
            assert!(
                started.elapsed() < Duration::from_millis(2150),
                "steady state below 28 fps"
            );
            eprintln!(
                "four {diameter}px videos: {count} frames in {:?}, CPU draw p95 {:?}",
                started.elapsed(),
                times[times.len() * 95 / 100]
            );
            if let Ok(dir) = std::env::var("ICU_OVERLAY_SHOTS") {
                let mut rgba = canvas.clone();
                for p in rgba.as_chunks_mut::<4>().0 {
                    p.swap(0, 2);
                }
                image::save_buffer(
                    std::path::Path::new(&dir).join(format!("nebula-{diameter}.png")),
                    &rgba,
                    w,
                    th,
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
            // The release finishes with no ghost halo; disabling the feature
            // must also suppress it even if an old envelope is still nonzero.
            let halo_y = cy - diameter / 2 - 6;
            let halo = ((halo_y * w + cx) * 4 + 3) as usize;
            assert!(canvas[halo] > 0, "visible halo above face");
            scene.intensities = &[0.; 4];
            renderer.invalidate();
            complete(&mut renderer, &mut scene, &mut canvas);
            assert_eq!(canvas[halo], 0, "release leaves no ghost halo");
            scene.intensities = &[1.; 4];
            scene.geometry.speaking_frame = false;
            renderer.invalidate();
            complete(&mut renderer, &mut scene, &mut canvas);
            assert_eq!(canvas[halo], 0, "disabled highlight has no halo");
            // No stale viewer reports may survive a same-size scene change.
            scene.visible_frames = &[(2, 9)];
            renderer.invalidate();
            assert!(renderer.draw(&mut scene, &mut canvas).unwrap().is_none());
            complete(&mut renderer, &mut scene, &mut canvas);
        }
        // Mixed camera/voice tiles, a vertical stack and a bottom-aligned row
        // at 200% scale. Their unequal slot sizes must never clip the label.
        for horizontal in [false, true] {
            let mut labels = Labels::new();
            let tiles = [
                Tile {
                    face: Face::Camera(None),
                    name: "Camera missing".into(),
                    speaking: false,
                    dim: true,
                    scale: 1.,
                },
                Tile {
                    face: Face::Plate,
                    name: "Voice only".into(),
                    speaking: true,
                    dim: false,
                    scale: REST_SCALE,
                },
            ];
            let geoms = [240, 144].map(|d| {
                let mut g = circle_geometry(if d == 240 { speak_diameter(d) } else { d }, 24);
                g.outer_w += HALO * 4;
                g.outer_h += HALO * 4;
                g
            });
            let w = if horizontal {
                geoms[0].outer_w + geoms[1].outer_w + GAP
            } else {
                geoms[0].outer_w.max(geoms[1].outer_w)
            };
            let h = if horizontal {
                geoms[0].outer_h.max(geoms[1].outer_h)
            } else {
                geoms[0].outer_h + geoms[1].outer_h + GAP
            };
            let mut scene = RenderScene {
                geometry: Geometry {
                    width: w,
                    height: h,
                    camera_diameter: 240,
                    plate_diameter: 144,
                    name_px: 24,
                    horizontal,
                    bottom: true,
                    speaking_frame: true,
                    frame_color: [46, 155, 255],
                },
                scale: 2.,
                tiles: &tiles,
                intensities: &[0., 1.],
                labels: &mut labels,
                visible_frames: &[],
            };
            renderer.invalidate();
            let mut canvas = vec![0; (w * h * 4) as usize];
            complete(&mut renderer, &mut scene, &mut canvas);
            assert!(
                canvas.as_chunks::<4>().0.iter().any(|p| p[..3] == [255; 3]),
                "voice initials rendered"
            );
            assert!(canvas
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[..3].iter().all(|c| *c <= p[3])));
            assert!(
                canvas[..(w * 4) as usize].iter().all(|v| *v == 0),
                "halo clipped at top"
            );
        }
        renderer.device.destroy();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !renderer.failed.load(Ordering::Relaxed) {
            let _ = renderer.device.poll(wgpu::PollType::Poll);
            assert!(
                Instant::now() < deadline,
                "device loss must trigger fallback"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
