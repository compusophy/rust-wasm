use wasm_bindgen::prelude::*;
use web_sys::{WebSocket, MessageEvent, ErrorEvent};
use wasm_bindgen::JsCast;

#[wasm_bindgen]
extern "C" {
    fn alert(s: &str);
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

#[wasm_bindgen]
pub fn greet() {
    alert("Hello from Rust compiled to WebAssembly!");
}

#[wasm_bindgen]
pub struct ChatClient {
    socket: WebSocket,
}

#[wasm_bindgen]
impl ChatClient {
    #[wasm_bindgen(constructor)]
    pub fn new(url: &str) -> Result<ChatClient, JsValue> {
        let ws = WebSocket::new(url)?;

        // -- ON MESSAGE CALLBACK --
        let onmessage_callback = Closure::wrap(Box::new(move |e: MessageEvent| {
            if let Ok(txt) = e.data().dyn_into::<js_sys::JsString>() {
                let text: String = txt.into();
                log(&format!("Received: {}", text));
                
                // Dispatch a custom event so htmx or JS can pick it up
                let window = web_sys::window().unwrap();
                let document = window.document().unwrap();
                
                // Simple DOM manipulation to show messages directly for now
                if let Some(container) = document.get_element_by_id("chat-messages") {
                    let p = document.create_element("div").unwrap();
                    p.set_text_content(Some(&text));
                    let _ = container.append_child(&p);
                }
            }
        }) as Box<dyn FnMut(MessageEvent)>);

        ws.set_onmessage(Some(onmessage_callback.as_ref().unchecked_ref()));
        onmessage_callback.forget();

        // -- ON ERROR CALLBACK --
        let onerror_callback = Closure::wrap(Box::new(move |e: ErrorEvent| {
            log(&format!("WebSocket error: {:?}", e));
        }) as Box<dyn FnMut(ErrorEvent)>);
        
        ws.set_onerror(Some(onerror_callback.as_ref().unchecked_ref()));
        onerror_callback.forget();

        Ok(ChatClient { socket: ws })
    }

    pub fn send_message(&self, msg: &str) -> Result<(), JsValue> {
        self.socket.send_with_str(msg)
    }
}
