use crate::{PluginError, Result};
use serde_json::Value;
use wasmi::{
    Caller, Config, EnforcedLimits, Engine, Instance, Linker, Module, Store, StoreLimits,
    StoreLimitsBuilder,
};

pub const MAX_MODULE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_INPUT_BYTES: usize = 64 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 256 * 1024;
pub const MAX_MEMORY_BYTES: usize = 64 * 1024 * 1024;
pub const EXECUTION_FUEL: u64 = 5_000_000;

fn runtime(error: impl std::fmt::Display) -> PluginError {
    PluginError::Runtime(error.to_string())
}

type HostCall = Box<dyn FnMut(Value) -> Result<Value> + Send>;
struct RuntimeState {
    limits: StoreLimits,
    host: Option<HostCall>,
    calls: usize,
}
const MAX_HOST_BYTES: usize = 64 * 1024;

fn host_call(
    mut caller: Caller<RuntimeState>,
    pointer: i32,
    length: i32,
    output: i32,
    capacity: i32,
) -> std::result::Result<i32, wasmi::Error> {
    let error = || wasmi::Error::new("Invalid host call buffer or call budget exhausted");
    if length <= 0
        || length as usize > MAX_HOST_BYTES
        || capacity <= 0
        || capacity as usize > MAX_HOST_BYTES
        || caller.data().calls >= 64
    {
        return Err(error());
    }
    let memory = caller
        .get_export("memory")
        .and_then(|value| value.into_memory())
        .ok_or_else(error)?;
    let output_start = output as u32 as usize;
    let output_end = output_start
        .checked_add(capacity as usize)
        .ok_or_else(error)?;
    if output_end > memory.data(&caller).len() {
        return Err(error());
    }
    let mut input = vec![0; length as usize];
    memory
        .read(&caller, pointer as u32 as usize, &mut input)
        .map_err(|_| error())?;
    let request: Value = serde_json::from_slice(&input)
        .map_err(|_| wasmi::Error::new("Invalid host request JSON"))?;
    caller.data_mut().calls += 1;
    let result = match caller.data_mut().host.as_mut() {
        Some(host) => host(request),
        None => Err(PluginError::Invalid(
            "Host capabilities are unavailable".into(),
        )),
    };
    let response = match result {
        Ok(value) => serde_json::json!({"ok":true,"value":value}),
        Err(error) => serde_json::json!({"ok":false,"error":error.to_string()}),
    };
    let response = serde_json::to_vec(&response).map_err(|_| error())?;
    if response.len() > capacity as usize {
        // A completed mutation must not be retried automatically when its
        // response does not fit. The SDK always supplies the full 64 KiB buffer.
        return Err(wasmi::Error::new("Host response exceeds the output buffer"));
    }
    memory
        .write(&mut caller, output_start, &response)
        .map_err(|_| error())?;
    Ok(response.len() as i32)
}

fn instantiate(bytes: &[u8], host: Option<HostCall>) -> Result<(Store<RuntimeState>, Instance)> {
    if bytes.len() > MAX_MODULE_BYTES || !bytes.starts_with(b"\0asm\x01\0\0\0") {
        return Err(PluginError::Invalid(
            "Plugin must be a WebAssembly v1 binary no larger than 8 MiB".into(),
        ));
    }
    let mut config = Config::default();
    config
        .consume_fuel(true)
        .allow_start_fn(false)
        .enforced_limits(EnforcedLimits::strict())
        .set_max_recursion_depth(256)
        .set_min_stack_height(16 * 1024)
        .set_max_stack_height(1024 * 1024)
        .set_max_cached_stacks(0);
    let engine = Engine::new(&config);
    let module = Module::new(&engine, bytes).map_err(runtime)?;
    if module
        .imports()
        .any(|import| import.module() != "ytu" || import.name() != "host_call")
    {
        return Err(PluginError::Invalid(
            "Only ytu.host_call is allowed; no WASI, filesystem, process or network imports".into(),
        ));
    }
    let limits = StoreLimitsBuilder::new()
        .memory_size(MAX_MEMORY_BYTES)
        .memories(1)
        .tables(1)
        .table_elements(4096)
        .instances(1)
        .trap_on_grow_failure(true)
        .build();
    let mut store = Store::new(
        &engine,
        RuntimeState {
            limits,
            host,
            calls: 0,
        },
    );
    store.limiter(|state| &mut state.limits);
    store.set_fuel(EXECUTION_FUEL).map_err(runtime)?;
    let mut linker = Linker::<RuntimeState>::new(&engine);
    linker
        .func_wrap("ytu", "host_call", host_call)
        .map_err(runtime)?;
    let instance = linker
        .instantiate_and_start(&mut store, &module)
        .map_err(runtime)?;
    instance
        .get_memory(&store, "memory")
        .ok_or_else(|| PluginError::Invalid("Plugin must export memory".into()))?;
    instance
        .get_typed_func::<i32, i32>(&store, "ytu_alloc")
        .map_err(runtime)?;
    instance
        .get_typed_func::<(i32, i32), i64>(&store, "ytu_command")
        .map_err(runtime)?;
    Ok((store, instance))
}

pub(crate) fn validate_module(bytes: &[u8]) -> Result<()> {
    instantiate(bytes, None).map(|_| ())
}

pub(crate) fn execute(bytes: &[u8], input: &Value, host: HostCall) -> Result<Value> {
    let input = serde_json::to_vec(input)?;
    if input.len() > MAX_INPUT_BYTES {
        return Err(PluginError::Invalid("Plugin input exceeds 64 KiB".into()));
    }
    let (mut store, instance) = instantiate(bytes, Some(host))?;
    let memory = instance
        .get_memory(&store, "memory")
        .ok_or_else(|| PluginError::Invalid("Missing plugin memory".into()))?;
    let allocate = instance
        .get_typed_func::<i32, i32>(&store, "ytu_alloc")
        .map_err(runtime)?;
    let command = instance
        .get_typed_func::<(i32, i32), i64>(&store, "ytu_command")
        .map_err(runtime)?;
    let pointer = allocate
        .call(&mut store, input.len() as i32)
        .map_err(runtime)?;
    memory
        .write(&mut store, pointer as u32 as usize, &input)
        .map_err(runtime)?;
    let packed = command
        .call(&mut store, (pointer, input.len() as i32))
        .map_err(runtime)? as u64;
    let output_pointer = (packed >> 32) as usize;
    let output_len = (packed & 0xffff_ffff) as usize;
    if output_len == 0 || output_len > MAX_OUTPUT_BYTES {
        return Err(PluginError::Invalid(
            "Plugin returned an empty response or more than 256 KiB".into(),
        ));
    }
    let mut output = vec![0; output_len];
    memory
        .read(&store, output_pointer, &mut output)
        .map_err(runtime)?;
    Ok(serde_json::from_slice(&output)?)
}
