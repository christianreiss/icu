//! Spike 4b: wlr-layer-shell Overlay: transparent, Layer Overlay, leere Input-Region, kein Keyboard-Fokus.
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure}, WaylandSurface},
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::{globals::registry_queue_init, protocol::{wl_output, wl_shm, wl_surface}, Connection, QueueHandle};
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let conn = Connection::connect_to_env()?;
    let (globals, mut eq) = registry_queue_init(&conn)?;
    let qh = eq.handle();
    let compositor = CompositorState::bind(&globals, &qh)?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| anyhow::anyhow!("kein zwlr_layer_shell_v1: {e}"))?;
    let shm = Shm::bind(&globals, &qh)?;
    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("icu-overlay"), None);
    layer.set_anchor(Anchor::TOP | Anchor::RIGHT);
    layer.set_size(320, 240);
    layer.set_margin(40, 40, 0, 0);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.set_exclusive_zone(-1);
    let empty = Region::new(&compositor)?;
    layer.set_input_region(Some(empty.wl_region()));
    layer.commit();
    let pool = SlotPool::new(320 * 240 * 4, &shm)?;
    let mut app = App { registry: RegistryState::new(&globals), outputs: OutputState::new(&globals, &qh), shm, pool, layer, configured: false, frames: 0, exit: false };
    let t0 = Instant::now();
    while !app.exit && t0.elapsed() < Duration::from_secs(6) {
        eq.blocking_dispatch(&mut app)?;
    }
    println!("[overlay] layer surface configured={} frames drawn={} (6 s sichtbar oben rechts, halbtransparent grün mit Loch)", app.configured, app.frames);
    Ok(())
}
struct App { registry: RegistryState, outputs: OutputState, shm: Shm, pool: SlotPool, layer: LayerSurface, configured: bool, frames: u32, exit: bool }
impl App {
    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let (w, h) = (320, 240);
        let (buf, canvas) = self.pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888).unwrap();
        for y in 0..h { for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let inside = (x - 160).pow(2) + (y - 120).pow(2) < 60 * 60;
            let (a, r, g, b) = if inside { (0u8, 0u8, 0u8, 0u8) } else { (160u8, 20u8, 160u8, 60u8) }; // premultiplied
            canvas[i] = b; canvas[i + 1] = g; canvas[i + 2] = r; canvas[i + 3] = a;
        } }
        self.layer.wl_surface().damage_buffer(0, 0, w, h);
        self.layer.wl_surface().frame(qh, self.layer.wl_surface().clone());
        buf.attach_to(self.layer.wl_surface()).unwrap();
        self.layer.commit();
        self.frames += 1;
    }
}
impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) { if self.frames < 3 { self.draw(qh); } }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}
impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState { &mut self.outputs }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}
impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) { self.exit = true; }
    fn configure(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &LayerSurface, c: LayerSurfaceConfigure, _: u32) {
        println!("[overlay] configure {:?}", c.new_size);
        self.configured = true;
        self.draw(qh);
    }
}
impl ShmHandler for App { fn shm_state(&mut self) -> &mut Shm { &mut self.shm } }
impl ProvidesRegistryState for App { fn registry(&mut self) -> &mut RegistryState { &mut self.registry } registry_handlers![OutputState]; }
delegate_compositor!(App); delegate_output!(App); delegate_shm!(App); delegate_layer!(App); delegate_registry!(App);
