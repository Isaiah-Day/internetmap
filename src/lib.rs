pub mod display;
mod forces;

pub const PUBLIC_ENDPOINT: &str = "https://public.api.bsky.app";

mod avatars;
#[cfg(not(target_arch = "wasm32"))]
pub mod bsky;
#[cfg(not(target_arch = "wasm32"))]
pub mod collect;
#[cfg(not(target_arch = "wasm32"))]
pub mod graph;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub struct WebHandle {
    runner: eframe::WebRunner,
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
impl WebHandle {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        console_error_panic_hook::set_once();
        Self {
            runner: eframe::WebRunner::new(),
        }
    }

    #[wasm_bindgen]
    pub async fn start(&self, canvas: web_sys::HtmlCanvasElement) -> Result<(), JsValue> {
        let app = display::MapApp::new()
            .await
            .map_err(|err| JsValue::from_str(&err.to_string()))?;
        self.runner
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(move |_cc| Ok(Box::new(app))),
            )
            .await
    }

    pub fn destroy(&self) {
        self.runner.destroy();
    }
}
