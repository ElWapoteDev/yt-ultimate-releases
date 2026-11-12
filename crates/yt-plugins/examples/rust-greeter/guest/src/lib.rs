//! Minimal Rust Plugin API v2 guest. The host creates a fresh instance per call,
//! so all allocations are reclaimed when the command returns.
use serde_json::{json,Value};

#[no_mangle]
pub extern "C" fn ytu_alloc(length:u32)->u32 {
    let mut buffer=Vec::<u8>::with_capacity(length as usize);
    let pointer=buffer.as_mut_ptr();
    std::mem::forget(buffer);
    pointer as u32
}

#[no_mangle]
/// # Safety
/// `pointer` must refer to `length` initialized bytes allocated in this
/// instance's linear memory by `ytu_alloc`.
pub unsafe extern "C" fn ytu_command(pointer:u32,length:u32)->u64 {
    let input=unsafe {std::slice::from_raw_parts(pointer as *const u8,length as usize)};
    let response=match serde_json::from_slice::<Value>(input) {
        Ok(request) if request["command"]=="greet"=>{
            let greeting=request["config"]["greeting"].as_str().unwrap_or("Hola");
            let name=request["arguments"]["name"].as_str().unwrap_or("mundo");
            let mut message=format!("{greeting}, {name}!");
            if request["config"]["uppercase"].as_bool()==Some(true) {message=message.to_uppercase();}
            json!({"message":message})
        }
        Ok(_)=>json!({"error":"Unknown command"}),
        Err(_)=>json!({"error":"Invalid request JSON"}),
    };
    let output=response.to_string().into_bytes().into_boxed_slice();
    let pointer=output.as_ptr() as u32;
    let length=output.len() as u32;
    std::mem::forget(output);
    ((pointer as u64)<<32) | length as u64
}
