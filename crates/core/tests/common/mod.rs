//! What every relay test needs: the relay's key on each request (`local::Guard`).
//!
//! Each relay a test starts is remembered with its key, and [`keyed`] adds the right one to a
//! request by the address it goes to — so the helpers that take a URL need no key passed in.

#![allow(dead_code)]

use std::sync::Mutex;

use lemmate_core::client::LocalHandle;

static KEYS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// Remember a relay's key, and hand the relay back.
pub fn remember(handle: LocalHandle) -> LocalHandle {
    KEYS.lock().unwrap().push((format!("//{}/", handle.addr), handle.key.clone()));
    handle
}

/// `req` with the key of the relay `url` points at, if it points at one.
pub fn keyed<B>(req: ureq::RequestBuilder<B>, url: &str) -> ureq::RequestBuilder<B> {
    let key =
        KEYS.lock().unwrap().iter().find(|(addr, _)| url.contains(addr.as_str())).map(|(_, k)| k.clone());
    match key {
        Some(k) => req.header("authorization", format!("Bearer {k}")),
        None => req,
    }
}
