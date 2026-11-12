//! Bounded JSON SDK. The host checks declared and explicitly granted permissions.
use serde_json::{json, Value};
#[link(wasm_import_module = "ytu")]
extern "C" {
    fn host_call(pointer: i32, length: i32, output: i32, capacity: i32) -> i32;
}

pub fn call(request: Value) -> Result<Value, String> {
    let input = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
    if input.len() > 64 * 1024 {
        return Err("Host request exceeds 64 KiB".into());
    }
    let mut output = vec![0u8; 64 * 1024];
    let length = unsafe {
        host_call(
            input.as_ptr() as i32,
            input.len() as i32,
            output.as_mut_ptr() as i32,
            output.len() as i32,
        )
    };
    if length <= 0 || length as usize > output.len() {
        return Err("Invalid host response".into());
    }
    let response: Value =
        serde_json::from_slice(&output[..length as usize]).map_err(|error| error.to_string())?;
    if response["ok"] == true {
        Ok(response["value"].clone())
    } else {
        Err(response["error"]
            .as_str()
            .unwrap_or("Host operation failed")
            .into())
    }
}
pub fn get(key: &str) -> Result<Value, String> {
    call(json!({"operation":"state.get","key":key}))
}
pub fn set(key: &str, value: Value) -> Result<(), String> {
    call(json!({"operation":"state.set","key":key,"value":value})).map(|_| ())
}
