use crate::{PluginError, Result};
use serde_json::Value;
use wasmi::{
    Config, EnforcedLimits, Engine, Instance, Linker, Module, Store, StoreLimits,
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

fn instantiate(bytes: &[u8]) -> Result<(Store<StoreLimits>, Instance)> {
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
    if module.imports().next().is_some() {
        return Err(PluginError::Invalid(
            "Plugin imports are disabled: no WASI, filesystem, process or network access".into(),
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
    let mut store = Store::new(&engine, limits);
    store.limiter(|limits| limits);
    store.set_fuel(EXECUTION_FUEL).map_err(runtime)?;
    let linker = Linker::<StoreLimits>::new(&engine);
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
    instantiate(bytes).map(|_| ())
}

pub(crate) fn execute(bytes: &[u8], input: &Value) -> Result<Value> {
    let input = serde_json::to_vec(input)?;
    if input.len() > MAX_INPUT_BYTES {
        return Err(PluginError::Invalid("Plugin input exceeds 64 KiB".into()));
    }
    let (mut store, instance) = instantiate(bytes)?;
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
