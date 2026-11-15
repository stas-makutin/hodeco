//! hodeco admin and diagnostics frontend.
//!
//! The crate is split into pure, testable modules (codec, endpoint parsing,
//! gateway state machine, demo fixtures) and the egui shell. The shell only
//! renders model state and sends tagged requests; it never speaks HTTP.

pub mod app;
pub mod client;
pub mod demo;
pub mod model;
pub mod runtime;
pub mod startup;
pub mod theme;
pub mod ui;

/// Set while the Messages view can handle F3.
#[cfg(target_arch = "wasm32")]
pub(crate) static FIND_SHORTCUT_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// F3 normally opens the browser's find bar before egui can handle the key.
/// While the Messages view is active, the page-level keydown handler prevents
/// that default so egui receives the key.
#[cfg(target_arch = "wasm32")]
fn install_find_shortcut_guard() {
    use std::sync::atomic::Ordering;
    use wasm_bindgen::JsCast;

    let Some(window) = web_sys::window() else {
        return;
    };
    let handler = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(
        |event: web_sys::KeyboardEvent| {
            if event.key() == "F3" && FIND_SHORTCUT_ACTIVE.load(Ordering::Relaxed) {
                event.prevent_default();
            }
        },
    );
    let _ = window.add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref());
    handler.forget();
}

/// Browser entry point. The generated JS loader calls the exported
/// `#[wasm_bindgen(start)]` function while initializing the module.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() -> Result<(), wasm_bindgen::JsValue> {
    use wasm_bindgen::JsCast;

    eframe::WebLogger::init(log::LevelFilter::Debug).ok();
    install_find_shortcut_guard();

    let startup = startup::from_web()
        .map_err(|error| wasm_bindgen::JsValue::from_str(&format!("startup error: {error}")))?;

    let canvas = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id("hodeco-canvas"))
        .and_then(|element| element.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        .ok_or_else(|| {
            wasm_bindgen::JsValue::from_str("canvas element #hodeco-canvas was not found")
        })?;

    wasm_bindgen_futures::spawn_local(async move {
        runtime::install_adjacent_web().await;
        let runner = eframe::WebRunner::new();
        let result = runner
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(move |cc| Ok(Box::new(app::App::new(cc, startup)))),
            )
            .await;
        if let Err(error) = result {
            log::error!("failed to start hodeco-ui: {error:?}");
        }
        // The runner is also referenced by the installed event callbacks, but
        // keep this handle alive for the lifetime of the page.
        std::mem::forget(runner);
    });

    Ok(())
}
