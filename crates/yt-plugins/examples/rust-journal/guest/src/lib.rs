//! Rust/Wasm workflow journal: scoped persistent state and subscribed events.
mod sdk;
use serde_json::{json, Value};

#[no_mangle]
pub extern "C" fn ytu_alloc(length: u32) -> u32 {
    let mut buffer = Vec::<u8>::with_capacity(length as usize);
    let pointer = buffer.as_mut_ptr();
    std::mem::forget(buffer);
    pointer as u32
}
#[no_mangle]
/// # Safety
/// Input must be initialized bytes in the guest allocation returned by ytu_alloc.
pub unsafe extern "C" fn ytu_command(pointer: u32, length: u32) -> u64 {
    let input = unsafe { std::slice::from_raw_parts(pointer as *const u8, length as usize) };
    let response = serde_json::from_slice::<Value>(input)
        .map_err(|error| error.to_string())
        .and_then(handle)
        .unwrap_or_else(|error| json!({"error":error}));
    let output = response.to_string().into_bytes().into_boxed_slice();
    let pointer = output.as_ptr() as u32;
    let length = output.len() as u32;
    std::mem::forget(output);
    ((pointer as u64) << 32) | length as u64
}
fn handle(request: Value) -> Result<Value, String> {
    let state = sdk::get("journal")?;
    let mut entries = state["entries"].as_array().cloned().unwrap_or_default();
    let mut total = state["total"].as_u64().unwrap_or(0);
    let mut progress = state["progress"].as_object().cloned().unwrap_or_default();
    let command = request["command"].as_str().unwrap_or("");
    match command {
        "__event__" => {
            let event = &request["event"];
            let name = event["name"].as_str().unwrap_or("");
            if let Some(allowed) = request["config"]["events"].as_array() {
                if !allowed.iter().any(|value| value == name) {
                    return Ok(json!({"recorded":false}));
                }
            }
            if name.ends_with(".progress") {
                let id = event["payload"]["job_id"].as_str().unwrap_or(name);
                let now = event["payload"]["progress"].as_f64().unwrap_or(0.0);
                let step = request["config"]["progress_step"].as_f64().unwrap_or(5.0);
                if let Some(last) = progress.get(id).and_then(Value::as_f64) {
                    if now < 100.0 && now - last < step {
                        return Ok(json!({"recorded":false}));
                    }
                }
                if progress.len() > 128 {
                    progress.clear();
                }
                progress.insert(id.into(), json!(now));
            }
            let payload = if request["config"]["include_payload"] == false {
                Value::Null
            } else {
                event["payload"].clone()
            };
            let payload = if payload.to_string().len() > 4096 {
                json!({"truncated":true})
            } else {
                payload
            };
            entries.push(json!({"sequence":total+1,"name":name,"payload":payload}));
            total += 1;
            let maximum = request["config"]["max_entries"]
                .as_u64()
                .unwrap_or(100)
                .clamp(1, 1000) as usize;
            while entries.len() > maximum
                || serde_json::to_vec(&entries).unwrap_or_default().len() > 40 * 1024
            {
                entries.remove(0);
            }
            sdk::set(
                "journal",
                json!({"entries":entries,"total":total,"progress":progress}),
            )?;
            Ok(json!({"recorded":true,"retained":entries.len(),"total":total}))
        }
        "summary" => Ok(
            json!({"message":format!("{} eventos guardados; {} recibidos",entries.len(),total),"retained":entries.len(),"total":total}),
        ),
        "recent" => {
            let limit = request["arguments"]["limit"]
                .as_u64()
                .unwrap_or(20)
                .min(1000) as usize;
            let start = entries.len().saturating_sub(limit);
            Ok(json!({"entries":&entries[start..],"total":total}))
        }
        "export" => Ok(
            json!({"jsonl":entries.iter().map(Value::to_string).collect::<Vec<_>>().join("\n"),"entries":entries.len()}),
        ),
        "clear" => {
            if request["arguments"]["confirmation"] != "BORRAR WORKFLOW JOURNAL" {
                return Err("Confirma con BORRAR WORKFLOW JOURNAL".into());
            }
            sdk::set("journal", json!({"entries":[],"total":0,"progress":{}}))?;
            Ok(json!({"message":"Journal borrado"}))
        }
        _ => Err("Unknown command".into()),
    }
}
