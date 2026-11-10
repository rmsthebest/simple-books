//! The browser's half of writing to a Kobo's drive, shared by the islands
//! that do: Chromium's File System Access API, called by name, and
//! `fetch`. Only in the browser's build.

use js_sys::{Array, Function, Object, Promise, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::Blob;

/// Calls a method by name. The File System Access API isn't in `web-sys`'s stable
/// bindings, and this needs no `unsafe` to declare them.
pub fn call(target: &JsValue, method: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let function: Function = Reflect::get(target, &JsValue::from_str(method))?.dyn_into()?;
    let list: Array = args.iter().collect();
    Reflect::apply(&function, target, &list)
}

pub async fn call_async(
    target: &JsValue,
    method: &str,
    args: &[JsValue],
) -> Result<JsValue, JsValue> {
    let promise: Promise = call(target, method, args)?.dyn_into()?;
    JsFuture::from(promise).await
}

pub fn options(key: &str, value: &JsValue) -> Result<JsValue, JsValue> {
    let object = Object::new();
    Reflect::set(&object, &JsValue::from_str(key), value)?;
    Ok(object.into())
}

pub fn error_name(error: &JsValue) -> String {
    Reflect::get(error, &JsValue::from_str("name"))
        .ok()
        .and_then(|name| name.as_string())
        .unwrap_or_default()
}

pub fn can_write_to_drive() -> bool {
    web_sys::window().is_some_and(|window| {
        Reflect::has(&window, &JsValue::from_str("showDirectoryPicker")).unwrap_or(false)
    })
}

/// What happened when asking for a drive.
pub enum Picked {
    Drive(JsValue),
    /// The person closed the picker.
    Cancelled,
    /// The browser won't open it, as for a whole drive on some systems.
    Refused,
}

/// Asks for a folder to read and write.
pub async fn pick_drive() -> Result<Picked, JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("No browser window"))?;
    let mode = options("mode", &JsValue::from_str("readwrite"))?;
    match call_async(&window, "showDirectoryPicker", &[mode]).await {
        Ok(root) => Ok(Picked::Drive(root)),
        Err(error) if error_name(&error) == "AbortError" => Ok(Picked::Cancelled),
        Err(error) if error_name(&error) == "SecurityError" => Ok(Picked::Refused),
        Err(error) => Err(error),
    }
}

pub async fn read_bytes(file: &Blob) -> Result<Vec<u8>, JsValue> {
    let buffer = JsFuture::from(file.array_buffer()).await?;
    Ok(Uint8Array::new(&buffer).to_vec())
}

pub async fn read_named(dir: &JsValue, name: &str) -> Result<Vec<u8>, JsValue> {
    let handle = call_async(dir, "getFileHandle", &[JsValue::from_str(name)]).await?;
    let file: Blob = call_async(&handle, "getFile", &[]).await?.dyn_into()?;
    read_bytes(&file).await
}

/// Whether `dir` has a file called `name`.
pub async fn has_file(dir: &JsValue, name: &str) -> bool {
    call_async(dir, "getFileHandle", &[JsValue::from_str(name)])
        .await
        .is_ok()
}

/// Writes the file, then reads it back. On a Kobo's drive a write once left an
/// empty file without any error, so the result is checked, not trusted.
pub async fn write_file(dir: &JsValue, name: &str, bytes: &[u8]) -> Result<bool, JsValue> {
    let create = options("create", &JsValue::TRUE)?;
    let handle = call_async(dir, "getFileHandle", &[JsValue::from_str(name), create]).await?;
    let writer = call_async(&handle, "createWritable", &[]).await?;
    let data: JsValue = Uint8Array::from(bytes).into();
    call_async(&writer, "write", &[data]).await?;
    call_async(&writer, "close", &[]).await?;
    Ok(read_named(dir, name).await? == bytes)
}

/// Removes the file `name` from `dir`, if it's there.
pub async fn remove_file(dir: &JsValue, name: &str) -> Result<(), JsValue> {
    if !has_file(dir, name).await {
        return Ok(());
    }
    call_async(dir, "removeEntry", &[JsValue::from_str(name)]).await?;
    Ok(())
}

pub async fn subdirectory(dir: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    call_async(dir, "getDirectoryHandle", &[JsValue::from_str(name)]).await
}

/// The folder at `path` under `dir`, made if it isn't there.
pub async fn made_directory(dir: &JsValue, path: &[&str]) -> Result<JsValue, JsValue> {
    let mut at = dir.clone();
    for name in path {
        let create = options("create", &JsValue::TRUE)?;
        at = call_async(
            &at,
            "getDirectoryHandle",
            &[JsValue::from_str(name), create],
        )
        .await?;
    }
    Ok(at)
}

/// A request to this site, with the person's sign-in. Returns the status and
/// the body.
pub async fn fetch(method: &str, path: &str) -> Result<(u16, Vec<u8>), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("No browser window"))?;
    let init = options("method", &JsValue::from_str(method))?;
    Reflect::set(
        &init,
        &JsValue::from_str("credentials"),
        &JsValue::from_str("same-origin"),
    )?;
    Reflect::set(
        &init,
        &JsValue::from_str("cache"),
        &JsValue::from_str("no-store"),
    )?;
    let response = call_async(&window, "fetch", &[JsValue::from_str(path), init]).await?;
    let status = Reflect::get(&response, &JsValue::from_str("status"))?
        .as_f64()
        .unwrap_or(0.0) as u16;
    let buffer = call_async(&response, "arrayBuffer", &[]).await?;
    Ok((status, Uint8Array::new(&buffer).to_vec()))
}
