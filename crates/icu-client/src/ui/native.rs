//! Native user actions must not depend on compositor redraw callbacks. In
//! particular Wayland stops those callbacks while a window is minimized.
use std::{
    cell::RefCell,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc, Mutex, Weak,
    },
    time::Instant,
};
use winit::{
    application::ApplicationHandler,
    event::{DeviceEvent, DeviceId, StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    window::{Window, WindowId},
};

#[cfg(target_os = "linux")]
use std::time::Duration;

const SHOW: u8 = 1;
const QUIT: u8 = 2;
static ACTIVE: Mutex<Option<Weak<Bridge>>> = Mutex::new(None);
thread_local! { static EVENT_LOOP: RefCell<Option<EventLoop<eframe::UserEvent>>> = const { RefCell::new(None) }; }

struct Bridge {
    proxy: EventLoopProxy<eframe::UserEvent>,
    pending: AtomicU8,
    #[cfg(target_os = "linux")]
    token: Mutex<Option<(String, Instant)>>,
    window: Mutex<Option<Weak<Window>>>,
}

fn wake_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("icu-native-actions")
}

/// Returns false outside a running native app (including headless UI tests).
pub(crate) fn dispatch(show: bool) -> bool {
    let Some(bridge) = ACTIVE.lock().unwrap().as_ref().and_then(Weak::upgrade) else {
        return false;
    };
    if bridge
        .pending
        .fetch_or(if show { SHOW } else { QUIT }, Ordering::Release)
        == 0
    {
        return bridge
            .proxy
            .send_event(eframe::UserEvent::RequestRepaint {
                viewport_id: wake_id(),
                when: Instant::now(),
                cumulative_pass_nr: 0,
            })
            .is_ok();
    }
    true
}

#[cfg(target_os = "linux")]
pub(crate) fn activation_token(token: String) {
    if token.is_empty() || token.len() > 4096 {
        return;
    }
    if let Some(bridge) = ACTIVE.lock().unwrap().as_ref().and_then(Weak::upgrade) {
        *bridge.token.lock().unwrap() = Some((token, Instant::now()));
    }
}

/// Both enrollment and the main window reuse this event loop after logout.
/// The factory still owns normal eframe initialization and engine cleanup.
pub(crate) fn run_native(
    name: &str,
    mut options: eframe::NativeOptions,
    creator: eframe::AppCreator<'_>,
) -> eframe::Result {
    EVENT_LOOP.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            let mut builder = EventLoop::with_user_event();
            if let Some(hook) = options.event_loop_builder.take() {
                hook(&mut builder);
            }
            *slot = Some(builder.build()?);
        }
        let event_loop = slot.as_mut().expect("initialized");
        let bridge = Arc::new(Bridge {
            proxy: event_loop.create_proxy(),
            pending: AtomicU8::new(0),
            #[cfg(target_os = "linux")]
            token: Mutex::new(None),
            window: Mutex::new(None),
        });
        *ACTIVE.lock().unwrap() = Some(Arc::downgrade(&bridge));
        let for_creator = bridge.clone();
        let creation = Arc::new(Mutex::new(None));
        let created = creation.clone();
        let app = eframe::create_native(
            name,
            options,
            Box::new(move |cc| {
                *for_creator.window.lock().unwrap() = cc.winit_window().map(Arc::downgrade);
                let result = creator(cc);
                *created.lock().unwrap() = Some(result.as_ref().map(|_| ()).map_err(ToString::to_string));
                result
            }),
            event_loop,
        );
        let mut app = NativeApp { inner: app, bridge };
        use winit::platform::run_on_demand::EventLoopExtRunOnDemand;
        let result = event_loop.run_app_on_demand(&mut app);
        *ACTIVE.lock().unwrap() = None;
        result?;
        let result = creation.lock().unwrap().take();
        match result {
            Some(Ok(())) => Ok(()),
            Some(Err(message)) => Err(eframe::Error::AppCreation(std::io::Error::other(message).into())),
            None => Err(eframe::Error::AppCreation(std::io::Error::other(
                "Native window/renderer could not initialize; see the graphics error in the log"
            ).into())),
        }
    })
}

struct NativeApp<'a> {
    inner: eframe::EframeWinitApplication<'a>,
    bridge: Arc<Bridge>,
}
impl ApplicationHandler<eframe::UserEvent> for NativeApp<'_> {
    fn resumed(&mut self, e: &ActiveEventLoop) {
        self.inner.resumed(e);
    }
    fn new_events(&mut self, e: &ActiveEventLoop, cause: StartCause) {
        self.inner.new_events(e, cause);
    }
    fn window_event(&mut self, e: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        self.inner.window_event(e, id, event);
    }
    fn device_event(&mut self, e: &ActiveEventLoop, id: DeviceId, event: DeviceEvent) {
        self.inner.device_event(e, id, event);
    }
    fn about_to_wait(&mut self, e: &ActiveEventLoop) {
        self.inner.about_to_wait(e);
    }
    fn suspended(&mut self, e: &ActiveEventLoop) {
        self.inner.suspended(e);
    }
    fn exiting(&mut self, e: &ActiveEventLoop) {
        self.inner.exiting(e);
    }
    fn memory_warning(&mut self, e: &ActiveEventLoop) {
        self.inner.memory_warning(e);
    }
    fn user_event(&mut self, e: &ActiveEventLoop, event: eframe::UserEvent) {
        if matches!(&event, eframe::UserEvent::RequestRepaint { viewport_id, .. } if *viewport_id == wake_id())
        {
            let actions = self.bridge.pending.swap(0, Ordering::Acquire);
            if actions & QUIT != 0 {
                e.exit();
                return;
            }
            if actions & SHOW != 0 {
                if let Some(window) = self
                    .bridge
                    .window
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(Weak::upgrade)
                {
                    window.set_minimized(false);
                    window.set_visible(true);
                    #[cfg(target_os = "linux")]
                    if let Some((token, received)) = self.bridge.token.lock().unwrap().take() {
                        if received.elapsed() <= Duration::from_secs(10) {
                            if let Err(error) = activate_wayland(&window, token) {
                                tracing::warn!(%error, "tray activation unavailable");
                            }
                        }
                    }
                    window.focus_window();
                    window.request_redraw();
                }
            }
        } else {
            self.inner.user_event(e, event);
        }
    }
}

#[cfg(target_os = "linux")]
fn activate_wayland(window: &Window, token: String) -> Result<(), Box<dyn std::error::Error>> {
    use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
    use wayland_client::{
        backend::{Backend, ObjectId},
        globals::{registry_queue_init, GlobalListContents},
        protocol::{wl_registry, wl_surface},
        Connection, Dispatch, Proxy, QueueHandle,
    };
    use wayland_protocols::xdg::activation::v1::client::xdg_activation_v1;
    let (RawDisplayHandle::Wayland(display), RawWindowHandle::Wayland(surface)) = (
        window.display_handle()?.as_raw(),
        window.window_handle()?.as_raw(),
    ) else {
        return Ok(());
    };
    struct Activation;
    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Activation {
        fn event(
            _: &mut Self,
            _: &wl_registry::WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }
    impl Dispatch<xdg_activation_v1::XdgActivationV1, ()> for Activation {
        fn event(
            _: &mut Self,
            _: &xdg_activation_v1::XdgActivationV1,
            _: xdg_activation_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }
    // SAFETY: these borrowed handles come from the same live winit window.
    // The event loop and window outlive this short-lived foreign-display view;
    // from_foreign_display never disconnects the owner's display on drop.
    let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
    let conn = Connection::from_backend(backend);
    let id = unsafe {
        ObjectId::from_ptr(
            wl_surface::WlSurface::interface(),
            surface.surface.as_ptr().cast(),
        )
    }?;
    let surface = wl_surface::WlSurface::from_id(&conn, id)?;
    let (globals, queue) = registry_queue_init::<Activation>(&conn)?;
    let activation: xdg_activation_v1::XdgActivationV1 =
        globals.bind(&queue.handle(), 1..=1, ())?;
    activation.activate(token, &surface);
    activation.destroy();
    conn.flush()?;
    Ok(())
}
